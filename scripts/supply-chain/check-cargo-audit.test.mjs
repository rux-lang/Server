import assert from 'node:assert/strict'
import test from 'node:test'

import { evaluateCargoAudit } from './check-cargo-audit.mjs'

function report(list = [], severity = 'high') {
  return {
    settings: { severity },
    vulnerabilities: { count: list.length, list }
  }
}

function finding({ cvss = null, id = 'RUSTSEC-2026-0001' } = {}) {
  return { advisory: { cvss, id, package: 'example' } }
}

test('accepts an empty high-severity report', () => {
  assert.deepEqual(evaluateCargoAudit(report()), { scored: [], unscored: [] })
})

test('classifies an unscored advisory as a warning', () => {
  const result = evaluateCargoAudit(report([finding()]))
  assert.equal(result.scored.length, 0)
  assert.equal(result.unscored.length, 1)
})

test('classifies a scored high-severity advisory as blocking', () => {
  const result = evaluateCargoAudit(report([finding({ cvss: 'CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H' })]))
  assert.equal(result.scored.length, 1)
  assert.equal(result.unscored.length, 0)
})

test('rejects reports generated with a weaker threshold', () => {
  assert.throws(() => evaluateCargoAudit(report([], 'medium')), /high severity threshold/)
})

test('rejects inconsistent report counts', () => {
  const value = report([finding()])
  value.vulnerabilities.count = 2
  assert.throws(() => evaluateCargoAudit(value), /count does not match/)
})
