import http from 'k6/http'
import exec from 'k6/execution'
import { check } from 'k6'

const profile = __ENV.RUX_PERFORMANCE_PROFILE ?? 'smoke'
const baseUrl = (__ENV.RUX_PERFORMANCE_API_BASE_URL ?? 'http://127.0.0.1:8080').replace(/\/$/, '')

if (!['smoke', 'launch'].includes(profile)) throw new Error(`unknown profile: ${profile}`)

const launch = profile === 'launch'
const warmupDuration = launch ? '60s' : '5s'
const measuredDuration = launch ? '5m' : '20s'
const readRate = launch ? 20 : 2
const searchRate = launch ? 5 : 1

export const options = {
  discardResponseBodies: true,
  scenarios: {
    warmup: {
      executor: 'constant-arrival-rate',
      exec: 'warmup',
      rate: launch ? 10 : 2,
      timeUnit: '1s',
      duration: warmupDuration,
      preAllocatedVUs: 4,
      maxVUs: 20,
      tags: { workload: 'warmup' }
    },
    reads: {
      executor: 'constant-arrival-rate',
      exec: 'readRequest',
      startTime: warmupDuration,
      rate: readRate,
      timeUnit: '1s',
      duration: measuredDuration,
      preAllocatedVUs: launch ? 16 : 2,
      maxVUs: launch ? 64 : 8,
      tags: { workload: 'read' }
    },
    search: {
      executor: 'constant-arrival-rate',
      exec: 'searchRequest',
      startTime: warmupDuration,
      rate: searchRate,
      timeUnit: '1s',
      duration: measuredDuration,
      preAllocatedVUs: launch ? 8 : 2,
      maxVUs: launch ? 32 : 8,
      tags: { workload: 'search' }
    }
  },
  thresholds: {
    checks: ['rate==1'],
    http_req_failed: ['rate<0.001'],
    dropped_iterations: ['count==0'],
    'http_req_duration{workload:read}': launch
      ? ['p(95)<250', 'p(99)<500']
      : ['p(95)<2000', 'p(99)<5000'],
    'http_req_duration{workload:search}': launch
      ? ['p(95)<500', 'p(99)<1000']
      : ['p(95)<3000', 'p(99)<6000']
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(95)', 'p(99)', 'max']
}

const reads = [
  ['/v1/index/n000001/p0000001', [200]],
  ['/v1/packages/n000001/p0000001', [200]],
  ['/v1/packages/n000001/p0000001/9.0.0', [200]],
  ['/v1/packages/n000001/p0000001/versions?limit=20', [200]],
  ['/v1/packages/n000001/p0000001/dependents?limit=20', [200]],
  ['/v1/highlights', [200]],
  ['/v1/keywords?limit=20', [200]],
  ['/v1/packages/n000001/p0000001/9.0.0/download', [302, 307]]
]

const searches = [
  '/v1/search?limit=20',
  '/v1/search?q=needle&limit=20',
  '/v1/search?q=p0000001&limit=20',
  '/v1/search?q=serialization&limit=20',
  '/v1/search?namespace=n000001&limit=20',
  '/v1/search?keyword=registry&limit=20',
  '/v1/search?q=definitely-absent&limit=20'
]

function request(path, accepted, workload) {
  const response = http.get(`${baseUrl}${path}`, {
    redirects: 0,
    tags: { workload, endpoint: path.split('?')[0] }
  })
  check(response, {
    [`${workload} response status is expected`]: value => accepted.includes(value.status)
  })
}

export function warmup() {
  request(searches[exec.scenario.iterationInTest % searches.length], [200], 'warmup')
}

export function readRequest() {
  const [path, accepted] = reads[exec.scenario.iterationInTest % reads.length]
  request(path, accepted, 'read')
}

export function searchRequest() {
  request(searches[exec.scenario.iterationInTest % searches.length], [200], 'search')
}
