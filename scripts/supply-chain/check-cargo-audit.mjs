import { readFile } from 'node:fs/promises'
import { pathToFileURL } from 'node:url'

export function evaluateCargoAudit(report) {
  if (!report || typeof report !== 'object') {
    throw new Error('Cargo audit report must be a JSON object')
  }
  if (report.settings?.severity !== 'high') {
    throw new Error('Cargo audit report was not generated with the high severity threshold')
  }

  const vulnerabilities = report.vulnerabilities
  if (!vulnerabilities || !Array.isArray(vulnerabilities.list)) {
    throw new Error('Cargo audit report is missing its vulnerability list')
  }
  if (vulnerabilities.count !== vulnerabilities.list.length) {
    throw new Error('Cargo audit vulnerability count does not match its list')
  }

  const scored = []
  const unscored = []
  for (const finding of vulnerabilities.list) {
    const advisory = finding?.advisory
    if (!advisory?.id || !advisory?.package) {
      throw new Error('Cargo audit report contains a malformed advisory')
    }
    if (advisory.cvss == null) unscored.push(advisory)
    else scored.push(advisory)
  }

  return { scored, unscored }
}

async function main() {
  const reportPath = process.argv[2]
  if (!reportPath) throw new Error('Usage: check-cargo-audit.mjs <cargo-audit.json>')

  const report = JSON.parse(await readFile(reportPath, 'utf8'))
  const { scored, unscored } = evaluateCargoAudit(report)

  for (const advisory of unscored) {
    console.warn(`Warning: ${advisory.id} affects ${advisory.package} but has no CVSS score`)
  }
  if (scored.length) {
    for (const advisory of scored) {
      console.error(`Error: ${advisory.id} affects ${advisory.package} (CVSS ${advisory.cvss})`)
    }
    process.exitCode = 1
    return
  }

  console.log(`Cargo audit policy passed (${unscored.length} unscored warning(s))`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main()
}
