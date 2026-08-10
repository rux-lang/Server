/**
 * The curated catalog the local seed is generated from.
 *
 * Names are PascalCase with no separators: the schema normalises with
 * `lower(replace(display_name, '_', '-'))`, so `HttpClient` becomes
 * `httpclient` and reads as one word everywhere in the UI.
 *
 * `category` only selects the flavour of the generated README and the shape of
 * the usage snippet; it is not persisted.
 */

export const NAMESPACES = [
  { name: "StdLib", author: "Rux Contributors", createdAt: "2026-01-01" },
  { name: "CommunityTools", author: "Rux Community", createdAt: "2026-01-02" },
  { name: "Acme", author: "Acme Tools Team", createdAt: "2026-01-03" },
  { name: "Northwind", author: "Northwind Systems", createdAt: "2026-01-05" },
  { name: "Helio", author: "Helio Labs", createdAt: "2026-01-08" },
  { name: "Cobalt", author: "Cobalt Works", createdAt: "2026-01-11" },
  { name: "Ironbark", author: "Ironbark Collective", createdAt: "2026-01-14" },
  { name: "Lumen", author: "Lumen Engineering", createdAt: "2026-01-17" },
  { name: "Meridian", author: "Meridian Data", createdAt: "2026-01-20" },
  { name: "Orbit", author: "Orbit Interactive", createdAt: "2026-01-23" },
  { name: "Sentinel", author: "Sentinel Security", createdAt: "2026-01-26" },
  { name: "Vantage", author: "Vantage Analytics", createdAt: "2026-01-29" },
];

/**
 * [namespace, Name, package_type, description, keywords, category, popularity]
 *
 * `popularity` (1-100) scales the generated download history so the highlights
 * endpoint has a believable ranking instead of a flat one.
 */
export const PACKAGES = [
  ["StdLib", "Io", "source_library", "Portable input and output primitives for Rux.", ["Io", "Runtime", "Streams"], "storage", 99],
  ["StdLib", "Json", "shared_library", "JSON parsing and serialization with a streaming reader.", ["Json", "Serialization", "Parsing"], "data", 97],
  ["StdLib", "Text", "shared_library", "UTF-8 string handling, slicing, and formatting.", ["Text", "Unicode", "Strings"], "text", 95],
  ["StdLib", "Math", "static_library", "Floating-point and integer math routines.", ["Math", "Numerics"], "math", 93],
  ["StdLib", "Memory", "source_library", "Allocation, copying, and arena helpers.", ["Memory", "Allocator"], "concurrency", 92],
  ["StdLib", "Time", "shared_library", "Instants, durations, and calendar conversion.", ["Time", "Clock", "Duration"], "data", 90],
  ["StdLib", "Collections", "shared_library", "Vectors, maps, sets, and deques with predictable growth.", ["Collections", "Containers"], "collections", 91],
  ["StdLib", "Process", "source_library", "Spawning, piping, and waiting on child processes.", ["Process", "Exec"], "storage", 78],
  ["StdLib", "Env", "shared_library", "Environment variables and process arguments.", ["Env", "Config"], "cli", 74],
  ["StdLib", "Path", "shared_library", "Cross-platform path parsing and joining.", ["Path", "Filesystem"], "storage", 82],

  ["CommunityTools", "HttpClient", "shared_library", "An HTTP/1.1 and HTTP/2 client with connection pooling.", ["Http", "Networking", "Client"], "net", 96],
  ["CommunityTools", "HttpServer", "shared_library", "A small, composable HTTP server.", ["Http", "Server", "Networking"], "net", 89],
  ["CommunityTools", "WebSocket", "shared_library", "RFC 6455 WebSocket client and server framing.", ["WebSocket", "Networking", "Realtime"], "net", 71],
  ["CommunityTools", "Router", "shared_library", "Radix-tree request routing with typed parameters.", ["Router", "Http", "Web"], "net", 68],
  ["CommunityTools", "Middleware", "shared_library", "Composable request middleware for HttpServer.", ["Middleware", "Http"], "net", 55],
  ["CommunityTools", "Uri", "shared_library", "URI parsing, normalization, and percent-encoding.", ["Uri", "Url", "Parsing"], "text", 73],
  ["CommunityTools", "Cookie", "shared_library", "Cookie jars and Set-Cookie parsing.", ["Cookie", "Http"], "net", 42],
  ["CommunityTools", "Mime", "shared_library", "MIME type sniffing and extension mapping.", ["Mime", "ContentType"], "data", 47],
  ["CommunityTools", "Multipart", "shared_library", "Streaming multipart/form-data reader and writer.", ["Multipart", "Upload", "Http"], "net", 39],
  ["CommunityTools", "Dns", "shared_library", "Asynchronous DNS resolution and record parsing.", ["Dns", "Networking"], "net", 44],

  ["Acme", "RegistryCli", "executable", "Command-line client for the Rux package registry.", ["Registry", "CommandLine", "Tooling"], "cli", 66],
  ["Acme", "ArgParse", "shared_library", "Declarative command-line argument parsing.", ["Cli", "Arguments", "Parsing"], "cli", 81],
  ["Acme", "Prompt", "shared_library", "Interactive prompts, confirmations, and selections.", ["Cli", "Prompt", "Interactive"], "cli", 58],
  ["Acme", "Progress", "shared_library", "Progress bars and spinners for terminal output.", ["Cli", "Progress", "Terminal"], "cli", 63],
  ["Acme", "Table", "shared_library", "Aligned table rendering for terminal output.", ["Cli", "Table", "Terminal"], "cli", 51],
  ["Acme", "Color", "shared_library", "ANSI colour and style composition.", ["Color", "Ansi", "Terminal"], "cli", 69],
  ["Acme", "Ansi", "source_library", "Low-level ANSI escape sequence primitives.", ["Ansi", "Terminal"], "cli", 37],
  ["Acme", "Watch", "shared_library", "Filesystem change watching with debouncing.", ["Watch", "Filesystem"], "storage", 46],

  ["Northwind", "Sqlite", "shared_library", "Bindings and a typed query layer for SQLite.", ["Sqlite", "Database", "Sql"], "storage", 86],
  ["Northwind", "Postgres", "shared_library", "An async PostgreSQL driver with prepared statements.", ["Postgres", "Database", "Sql"], "storage", 88],
  ["Northwind", "Redis", "shared_library", "RESP3 client with pipelining and pub/sub.", ["Redis", "Cache", "Database"], "storage", 76],
  ["Northwind", "Migrate", "executable", "Versioned, reviewable SQL migrations.", ["Migrations", "Database", "Tooling"], "storage", 54],
  ["Northwind", "Pool", "shared_library", "Generic connection pooling with health checks.", ["Pool", "Connections"], "concurrency", 61],
  ["Northwind", "Sql", "shared_library", "SQL statement building without string concatenation.", ["Sql", "Query", "Builder"], "storage", 49],
  ["Northwind", "Kv", "shared_library", "An embedded key-value store with an LSM backend.", ["Kv", "Storage", "Embedded"], "storage", 43],
  ["Northwind", "Blob", "shared_library", "S3-compatible object storage access.", ["Blob", "Storage", "S3"], "storage", 52],
  ["Northwind", "Cache", "shared_library", "In-process caching with TTL and size bounds.", ["Cache", "Memory"], "collections", 59],
  ["Northwind", "Lru", "source_library", "A fixed-capacity least-recently-used map.", ["Lru", "Cache", "Collections"], "collections", 41],

  ["Helio", "Zlib", "static_library", "Deflate and gzip compression and decompression.", ["Compression", "Gzip", "Deflate"], "data", 77],
  ["Helio", "Zstd", "shared_library", "Zstandard compression with dictionary support.", ["Compression", "Zstd"], "data", 64],
  ["Helio", "Tar", "shared_library", "Streaming tar archive reader and writer.", ["Archive", "Tar"], "data", 48],
  ["Helio", "Zip", "shared_library", "ZIP archive reading and writing.", ["Archive", "Zip"], "data", 53],
  ["Helio", "Base64", "source_library", "Base64 and base64url encoding.", ["Base64", "Encoding"], "data", 72],
  ["Helio", "Hex", "source_library", "Hexadecimal encoding and decoding.", ["Hex", "Encoding"], "data", 45],
  ["Helio", "Csv", "shared_library", "RFC 4180 CSV reading and writing.", ["Csv", "Parsing", "Data"], "data", 62],
  ["Helio", "Toml", "shared_library", "TOML parsing with span-accurate errors.", ["Toml", "Config", "Parsing"], "data", 70],
  ["Helio", "Yaml", "shared_library", "A YAML 1.2 parser and emitter.", ["Yaml", "Config", "Parsing"], "data", 57],
  ["Helio", "MsgPack", "shared_library", "MessagePack serialization.", ["MessagePack", "Serialization"], "data", 33],

  ["Cobalt", "Regex", "static_library", "A linear-time regular expression engine.", ["Regex", "Text", "Matching"], "text", 87],
  ["Cobalt", "Glob", "source_library", "Shell-style glob matching for paths.", ["Glob", "Matching", "Path"], "text", 50],
  ["Cobalt", "Diff", "shared_library", "Myers diff over lines and characters.", ["Diff", "Text"], "text", 40],
  ["Cobalt", "Markdown", "shared_library", "CommonMark parsing and HTML rendering.", ["Markdown", "CommonMark", "Text"], "text", 67],
  ["Cobalt", "Html", "shared_library", "An HTML5 tokenizer and tree builder.", ["Html", "Parsing"], "text", 56],
  ["Cobalt", "Xml", "shared_library", "A streaming XML pull parser.", ["Xml", "Parsing"], "text", 35],
  ["Cobalt", "Template", "shared_library", "A logic-light text templating engine.", ["Template", "Rendering"], "text", 38],
  ["Cobalt", "Unicode", "shared_library", "Unicode segmentation, casing, and normalization.", ["Unicode", "Text"], "text", 60],

  ["Ironbark", "Channel", "shared_library", "Bounded and unbounded message channels.", ["Channel", "Concurrency"], "concurrency", 84],
  ["Ironbark", "Executor", "shared_library", "A work-stealing task executor.", ["Executor", "Async", "Concurrency"], "concurrency", 83],
  ["Ironbark", "Scheduler", "shared_library", "Cron and interval task scheduling.", ["Scheduler", "Cron"], "concurrency", 47],
  ["Ironbark", "ThreadPool", "shared_library", "A fixed-size thread pool with graceful shutdown.", ["ThreadPool", "Concurrency"], "concurrency", 65],
  ["Ironbark", "Atomic", "source_library", "Atomic integers, flags, and fences.", ["Atomic", "Concurrency"], "concurrency", 55],
  ["Ironbark", "Mutex", "source_library", "Mutexes, read-write locks, and condition variables.", ["Mutex", "Locking", "Concurrency"], "concurrency", 58],
  ["Ironbark", "Actor", "shared_library", "A lightweight actor runtime with supervision.", ["Actor", "Concurrency"], "concurrency", 31],
  ["Ironbark", "Retry", "shared_library", "Retry policies with jittered exponential backoff.", ["Retry", "Resilience"], "concurrency", 44],
  ["Ironbark", "RateLimit", "shared_library", "Token-bucket and leaky-bucket rate limiting.", ["RateLimit", "Resilience"], "concurrency", 36],
  ["Ironbark", "Arena", "source_library", "A bump allocator for short-lived allocations.", ["Arena", "Allocator", "Memory"], "concurrency", 42],

  ["Lumen", "Logger", "shared_library", "Structured, levelled logging with pluggable sinks.", ["Logging", "Observability"], "observability", 85],
  ["Lumen", "Tracing", "shared_library", "Span-based tracing with OpenTelemetry export.", ["Tracing", "Telemetry", "Observability"], "observability", 74],
  ["Lumen", "Metrics", "shared_library", "Counters, gauges, and histograms with a Prometheus scrape endpoint.", ["Metrics", "Prometheus"], "observability", 75],
  ["Lumen", "Profiler", "executable", "A sampling CPU profiler with flamegraph output.", ["Profiler", "Performance"], "observability", 34],
  ["Lumen", "Bench", "shared_library", "Statistical benchmarking with outlier detection.", ["Benchmark", "Performance"], "testing", 46],
  ["Lumen", "Assert", "source_library", "Expressive assertions with readable failure output.", ["Assert", "Testing"], "testing", 63],
  ["Lumen", "Mock", "shared_library", "Test doubles and call verification.", ["Mock", "Testing"], "testing", 39],
  ["Lumen", "Fuzz", "executable", "Coverage-guided fuzzing for Rux targets.", ["Fuzz", "Testing", "Security"], "testing", 30],
  ["Lumen", "Coverage", "executable", "Line and branch coverage reporting.", ["Coverage", "Testing"], "testing", 32],

  ["Meridian", "BigInt", "shared_library", "Arbitrary-precision integer arithmetic.", ["BigInt", "Math", "Numerics"], "math", 51],
  ["Meridian", "Decimal", "shared_library", "Fixed-point decimals for financial arithmetic.", ["Decimal", "Math", "Finance"], "math", 57],
  ["Meridian", "Matrix", "shared_library", "Dense and sparse matrix operations.", ["Matrix", "LinearAlgebra", "Math"], "math", 48],
  ["Meridian", "Vector", "source_library", "Small-vector geometry for 2D and 3D.", ["Vector", "Geometry", "Math"], "math", 43],
  ["Meridian", "Simd", "source_library", "Portable SIMD intrinsics.", ["Simd", "Performance"], "math", 40],
  ["Meridian", "Random", "shared_library", "Seedable pseudo-random and cryptographic generators.", ["Random", "Prng"], "math", 79],
  ["Meridian", "Stats", "shared_library", "Descriptive statistics and distributions.", ["Statistics", "Math"], "math", 37],

  ["Orbit", "Graph", "shared_library", "Directed and undirected graphs with traversal algorithms.", ["Graph", "Algorithms", "Collections"], "collections", 45],
  ["Orbit", "Heap", "source_library", "Binary and pairing heaps.", ["Heap", "Collections"], "collections", 28],
  ["Orbit", "RingBuffer", "source_library", "A lock-free single-producer ring buffer.", ["RingBuffer", "Collections", "Concurrency"], "collections", 34],
  ["Orbit", "Bitset", "source_library", "A dense bitset with fast population counts.", ["Bitset", "Collections"], "collections", 29],
  ["Orbit", "Slab", "source_library", "A pre-allocated slab with stable handles.", ["Slab", "Memory", "Collections"], "collections", 26],
  ["Orbit", "Semver", "shared_library", "Semantic version parsing and range matching.", ["Semver", "Versioning"], "data", 66],
  ["Orbit", "Uuid", "source_library", "UUID v4 and v7 generation and parsing.", ["Uuid", "Identifiers"], "data", 80],

  ["Sentinel", "Sha256", "source_library", "SHA-256 and SHA-512 hashing.", ["Hashing", "Sha2", "Crypto"], "crypto", 82],
  ["Sentinel", "Blake3", "shared_library", "BLAKE3 hashing with parallel chunking.", ["Hashing", "Blake3", "Crypto"], "crypto", 61],
  ["Sentinel", "Argon2", "shared_library", "Argon2id password hashing.", ["Password", "Hashing", "Crypto"], "crypto", 54],
  ["Sentinel", "Jwt", "shared_library", "JSON Web Token signing and verification.", ["Jwt", "Auth", "Crypto"], "crypto", 71],
  ["Sentinel", "Tls", "shared_library", "A TLS 1.3 implementation with certificate verification.", ["Tls", "Crypto", "Networking"], "crypto", 68],
  ["Sentinel", "Ed25519", "source_library", "Ed25519 signatures.", ["Signatures", "Crypto"], "crypto", 44],
  ["Sentinel", "Aes", "source_library", "AES-GCM authenticated encryption.", ["Encryption", "Aes", "Crypto"], "crypto", 49],
  ["Sentinel", "Totp", "shared_library", "Time-based one-time passwords.", ["Totp", "Auth"], "crypto", 27],

  ["Vantage", "Config", "shared_library", "Layered configuration from files, environment, and flags.", ["Config", "Settings"], "cli", 73],
  ["Vantage", "Grpc", "shared_library", "gRPC client and server over HTTP/2.", ["Grpc", "Rpc", "Networking"], "net", 59],
  ["Vantage", "Protobuf", "shared_library", "Protocol Buffers encoding and code generation.", ["Protobuf", "Serialization"], "data", 56],
];
