import http from 'k6/http'
import exec from 'k6/execution'
import { check } from 'k6'
import { Counter } from 'k6/metrics'

const profile = __ENV.RUX_PERFORMANCE_PROFILE ?? 'smoke'
const baseUrl = (__ENV.RUX_PERFORMANCE_API_BASE_URL ?? 'http://127.0.0.1:8080').replace(/\/$/, '')
const fixtureDirectory = __ENV.RUX_PERFORMANCE_FIXTURES ?? './.performance/fixtures'

if (!['smoke', 'launch'].includes(profile)) throw new Error(`unknown profile: ${profile}`)

const fixtureManifest = JSON.parse(open(`${fixtureDirectory}/fixtures.json`))
if (fixtureManifest.profile !== profile) {
  throw new Error(`fixture profile ${fixtureManifest.profile} does not match ${profile}`)
}

function loadFixtures(values) {
  return values.map(value => ({
    ...value,
    manifest: open(`${fixtureDirectory}/${value.manifest_path}`),
    package: open(`${fixtureDirectory}/${value.package_path}`, 'b')
  }))
}

const typical = loadFixtures(fixtureManifest.typical)
const large = loadFixtures(fixtureManifest.large)
const launch = profile === 'launch'
const fixtureErrors = new Counter('fixture_errors')

export const options = {
  discardResponseBodies: true,
  scenarios: {
    typical: {
      executor: 'constant-arrival-rate',
      exec: 'publishTypical',
      rate: launch ? 4 : 6,
      timeUnit: '1m',
      duration: launch ? '4m59s' : '19s',
      preAllocatedVUs: 2,
      maxVUs: 4,
      tags: { workload: 'publication_typical' }
    },
    large: {
      executor: 'constant-arrival-rate',
      exec: 'publishLarge',
      startTime: launch ? '5m10s' : '25s',
      rate: launch ? 2 : 3,
      timeUnit: '1m',
      duration: launch ? '4m59s' : '19s',
      preAllocatedVUs: 2,
      maxVUs: 4,
      tags: { workload: 'publication_large' }
    }
  },
  thresholds: {
    checks: ['rate==1'],
    http_req_failed: ['rate<0.001'],
    dropped_iterations: ['count==0'],
    fixture_errors: ['count==0'],
    'http_req_duration{workload:publication_typical}': launch
      ? ['p(95)<5000', 'max<10000']
      : ['p(95)<30000', 'max<30000'],
    'http_req_duration{workload:publication_large}': launch
      ? ['p(95)<10000', 'max<15000']
      : ['p(95)<30000', 'max<30000']
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(95)', 'p(99)', 'max']
}

function publish(fixtures, workload) {
  const index = exec.scenario.iterationInTest
  const fixture = fixtures[index]
  if (!fixture) {
    fixtureErrors.add(1)
    throw new Error(`${workload} produced more iterations than generated fixtures`)
  }

  const response = http.post(`${baseUrl}/v1/packages`, {
    manifest: http.file(fixture.manifest, 'Rux.toml', 'text/plain'),
    package: http.file(fixture.package, `${fixture.version}.ruxpkg`, 'application/octet-stream')
  }, {
    headers: { Authorization: `Bearer ${fixtureManifest.bearer_token}` },
    tags: { workload }
  })
  check(response, {
    [`${workload} publication is created`]: value => value.status === 201
  })
}

export function publishTypical() {
  publish(typical, 'publication_typical')
}

export function publishLarge() {
  publish(large, 'publication_large')
}
