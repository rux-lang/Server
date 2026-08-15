/**
 * Generates `local-catalog.sql` from the curated list in `packages.mjs`.
 *
 *     node tools/catalog/generate-catalog.mjs
 *
 * The seed is written rather than hand-maintained because it carries 100
 * packages with several hundred versions, READMEs, keywords, dependencies and a
 * download history — too much to keep correct by hand, and every row has to
 * satisfy the CHECK constraints in `migrations/20260801000000_initial_schema.up.sql`.
 *
 * Output is deterministic: the PRNG is seeded from each package's identity, so
 * regenerating without editing `packages.mjs` produces a byte-identical file
 * and the counts asserted in `crates/infrastructure/tests/local_catalog_seed.rs`
 * stay stable.
 *
 * Download timestamps are the one exception — they are emitted relative to
 * `now()` so the 30-day highlights window always has data, however long after
 * generation the seed is loaded.
 */

import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { NAMESPACES, PACKAGES } from "./packages.mjs";

const OUTPUT = fileURLToPath(new URL("./local-catalog.sql", import.meta.url));

/** Total download rows to distribute across every version, by popularity. */
const TOTAL_DOWNLOAD_EVENTS = 500_000;
const DOWNLOAD_WINDOW_DAYS = 90;
const MIN_RUX = "0.4.0";
const LOCAL_CATALOG_CUTOFF = new Date(Date.UTC(2026, 7, 1));
const YANK_DELAY_DAYS = 20;

// ---------------------------------------------------------------------------
// Deterministic randomness
// ---------------------------------------------------------------------------

/** FNV-1a, used to derive a stable seed from a package's identity. */
function hash(text) {
  let value = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    value ^= text.charCodeAt(index);
    value = Math.imul(value, 0x01000193) >>> 0;
  }
  return value >>> 0;
}

/** mulberry32 — small, fast, and stable across Node versions. */
function rng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const pick = (random, items) => items[Math.floor(random() * items.length)];
const between = (random, low, high) => low + Math.floor(random() * (high - low + 1));

// ---------------------------------------------------------------------------
// SQL literals
// ---------------------------------------------------------------------------

const quote = (value) => `'${String(value).replace(/'/g, "''")}'`;
const nullable = (value) => (value === null || value === undefined ? "NULL" : quote(value));

/**
 * Dollar-quoting keeps embedded newlines and quotes intact. READMEs and
 * manifests are the only values large enough to need it; the tag is checked so
 * a future edit cannot silently terminate the literal early.
 */
function dollar(value, tag) {
  const delimiter = `$${tag}$`;
  if (String(value).includes(delimiter)) {
    throw new Error(`value contains its own dollar-quote delimiter ${delimiter}`);
  }
  return `${delimiter}${value}${delimiter}`;
}

const normalize = (name) => name.toLowerCase().replace(/_/g, "-");

/** 64 hex characters, so `decode(..., 'hex')` yields the required 32 bytes. */
function sha256Hex(text) {
  let out = "";
  let salt = 0;
  while (out.length < 64) {
    out += hash(`${text}:${salt}`).toString(16).padStart(8, "0");
    salt += 1;
  }
  return out.slice(0, 64);
}

const isoDay = (date) => date.toISOString().slice(0, 10);

// ---------------------------------------------------------------------------
// README composition
// ---------------------------------------------------------------------------

const USAGE = {
  net: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let client = ${name}::Open("https://example.com");\n    let response = client.Get("/status")?;\n    Print(response.Body());\n    return 0;\n}`,
  data: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let document = ${name}::Parse(input)?;\n    Print(${name}::Render(document));\n    return 0;\n}`,
  crypto: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let digest = ${name}::Hash(message);\n    Print(digest.ToHex());\n    return 0;\n}`,
  cli: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let app = ${name}::New("example");\n    app.Run(Env::Args())?;\n    return 0;\n}`,
  concurrency: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let handle = ${name}::Spawn(Work);\n    handle.Join()?;\n    return 0;\n}`,
  collections: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    var store = ${name}::WithCapacity(1024);\n    store.Insert("key", 42);\n    Print(store.Get("key")?);\n    return 0;\n}`,
  text: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let result = ${name}::Compile("^a.*z$")?;\n    Print(result.Matches("abcz"));\n    return 0;\n}`,
  testing: (ns, name) => `import ${ns}::${name};\n\ntest "adds numbers" {\n    ${name}::Equal(2 + 2, 4);\n}`,
  math: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let value = ${name}::From(1024);\n    Print(value.Pow(3).ToString());\n    return 0;\n}`,
  storage: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    let handle = ${name}::Open("local")?;\n    defer handle.Close();\n    Print(handle.Stat()?.Size);\n    return 0;\n}`,
  observability: (ns, name) => `import ${ns}::${name};\n\nfunc Main() -> int {\n    ${name}::Init(${name}::Config::Default());\n    ${name}::Info("service started");\n    return 0;\n}`,
};

const FEATURES = {
  net: ["Connection reuse with configurable pool limits", "Deadlines and cancellation on every call", "Streaming request and response bodies", "Automatic redirect and retry handling"],
  data: ["Zero-copy parsing where the input allows it", "Errors carry byte offsets into the source", "Streaming and whole-document modes", "Round-trips without losing key order"],
  crypto: ["Constant-time comparison helpers", "Misuse-resistant API defaults", "Test vectors from the reference specification", "No allocation on the hot path"],
  cli: ["Helpful errors instead of usage dumps", "Respects NO_COLOR and non-TTY output", "Subcommands with inherited options", "Shell completion generation"],
  concurrency: ["Bounded queues with backpressure", "Graceful shutdown that drains in flight work", "No hidden global runtime", "Cancellation propagates to children"],
  collections: ["Predictable growth with reserved capacity", "Iterators that borrow rather than copy", "Stable ordering guarantees where documented", "Compact memory layout"],
  text: ["Correct handling of multi-byte code points", "Linear-time matching with no backtracking", "Borrowed slices instead of copies", "Locale-independent behaviour by default"],
  testing: ["Failure output shows both values", "Deterministic ordering for reproducible runs", "Integrates with the built-in test runner", "No macros or code generation"],
  math: ["Exact results with no silent truncation", "Overflow is reported, never wrapped", "Const-evaluable where the compiler allows", "Documented rounding behaviour"],
  storage: ["Explicit resource lifetimes with defer", "Errors distinguish transient from permanent", "Cross-platform path handling", "Streaming reads for large payloads"],
  observability: ["Structured fields, not formatted strings", "Sampling to bound overhead in production", "Pluggable sinks and exporters", "Negligible cost when disabled"],
};

const OPENERS = [
  (desc) => `${desc}`,
  (desc) => `${desc} The API is small on purpose and does not pull in a runtime of its own.`,
  (desc) => `${desc} It is designed to be predictable under load and cheap to depend on.`,
  (desc) => `${desc} Everything is explicit: no hidden allocation, no global state.`,
];

const NOTES = [
  "This package targets the current stable Rux release and follows semantic versioning.",
  "Breaking changes are confined to major releases; deprecations are kept for one minor cycle.",
  "The public surface is covered by tests that run on Linux, macOS, and Windows.",
  "Benchmarks live in the repository and run as part of continuous integration.",
];

const DESIGN = [
  "The design goal is to stay out of the way. There is no initialization step, no global registry, and no background thread started on your behalf — you construct what you need, use it, and drop it. Anything that allocates says so in its name, and anything that can fail returns a result rather than trapping.",
  "Everything here is built around borrowed data. Functions take slices instead of owned buffers wherever they can, so callers decide when memory is copied and when it is reused. That makes the cost model obvious at the call site and keeps the package usable from allocation-free contexts.",
  "The implementation favours a single obvious path over a configurable one. Where a knob would only exist to paper over a bad default, the default was fixed instead. The result is a smaller API surface, fewer states to test, and behaviour that is the same on every platform.",
  "Errors are values, and they carry enough context to act on: what was expected, what was found, and where. Nothing is reported as a bare boolean, and nothing is silently discarded. Callers that only care whether an operation succeeded can still ignore the detail without paying for it.",
];

const COMPATIBILITY = [
  "Requires Rux 0.4.0 or later. Earlier releases lack the interface features this package depends on.",
  "Builds on Linux, macOS, Windows, and the BSDs. The Windows path uses the platform APIs directly rather than a compatibility shim.",
  "No external system libraries are required; the package compiles from source with the standard toolchain.",
  "Tested against the last two stable Rux releases. Nightly builds are exercised in continuous integration but not supported.",
];

const CONTRIBUTING = [
  "Bug reports and pull requests are welcome. Please open an issue before starting substantial work so the approach can be agreed first.",
  "Contributions are accepted under the same licence as the package. Run the test suite and the formatter before opening a pull request.",
];

/**
 * Composes a README between roughly 20 and 300 words.
 *
 * Three size tiers, because a registry that renders only medium-length READMEs
 * never exercises either end of the layout: a fifth are terse, a third are long
 * enough to need scrolling, and the rest sit in between.
 */
function readme(random, { namespace, name, description, category, keywords }) {
  const title = `# ${namespace}::${name}`;
  const opener = pick(random, OPENERS)(description);
  const install = ["## Installation", "", "```sh", `rux add ${namespace}::${name}`, "```", ""];

  const tier = random();

  if (tier < 0.2) {
    return [
      title,
      "",
      opener,
      "",
      ...install,
      `See the [documentation](https://rux-lang.dev/packages/${normalize(namespace)}/${normalize(name)}) for the full API. Released under the MIT License.`,
      "",
    ].join("\n");
  }

  const features = [...FEATURES[category]];
  const chosen = [];
  const count = tier < 0.65 ? between(random, 3, 4) : 4;
  for (let index = 0; index < count && features.length > 0; index += 1) {
    chosen.push(features.splice(Math.floor(random() * features.length), 1)[0]);
  }

  const usage = (USAGE[category] ?? USAGE.data)(namespace, name);
  const sections = [
    title,
    "",
    opener,
    "",
    ...install,
    "## Usage",
    "",
    "```rux",
    usage,
    "```",
    "",
    "## Features",
    "",
    ...chosen.map((feature) => `- ${feature}`),
    "",
  ];

  if (tier >= 0.65) {
    sections.push("## Design", "", pick(random, DESIGN), "");
    sections.push("## Compatibility", "", pick(random, COMPATIBILITY), "");
    sections.push("## Contributing", "", pick(random, CONTRIBUTING), "");
  } else if (random() < 0.6) {
    sections.push("## Notes", "", pick(random, NOTES), "");
  }

  sections.push(
    "## Topics",
    "",
    keywords.map((keyword) => `\`${keyword}\``).join(", "),
    "",
    "## License",
    "",
    "Released under the MIT License.",
    "",
  );

  return sections.join("\n");
}

// ---------------------------------------------------------------------------
// Version history
// ---------------------------------------------------------------------------

const LICENSES = ["MIT", "MIT OR Apache-2.0", "Apache-2.0", "BSD-3-Clause"];

/**
 * Builds 2-10 releases per package. The history walks forward from a start date
 * so `published_at` is monotonic, which is what the version list and the
 * "recently published" highlight both order by.
 */
function versionsFor(random, popularity) {
  const total = between(random, 2, 10);
  const releases = [];

  let major = random() < 0.35 ? 0 : 1;
  let minor = 0;
  let patch = 0;
  // Older, more popular packages started earlier; newer ones cluster near now.
  let day = new Date(Date.UTC(2026, 0, 15));
  day.setUTCDate(day.getUTCDate() + between(random, 0, 120 - Math.floor(popularity / 2)));

  for (let index = 0; index < total; index += 1) {
    const isLast = index === total - 1;
    const roll = random();

    if (index > 0) {
      if (roll < 0.2) {
        major += 1;
        minor = 0;
        patch = 0;
      } else if (roll < 0.6) {
        minor += 1;
        patch = 0;
      } else {
        patch += 1;
      }
    }

    // A prerelease only makes sense ahead of a final release, never as the tip.
    const prerelease = !isLast && index > 0 && random() < 0.18 ? `rc.${between(random, 1, 3)}` : null;
    const buildMetadata = random() < 0.08 ? pick(random, ["native", "portable", "musl"]) : null;

    let version = `${major}.${minor}.${patch}`;
    if (prerelease) version += `-${prerelease}`;
    if (buildMetadata) version += `+${buildMetadata}`;

    releases.push({
      version,
      major,
      minor,
      patch,
      prerelease,
      buildMetadata,
      publishedAt: new Date(day),
      // Only ever yank something that has a successor to move to.
      yanked: !isLast && random() < 0.06,
    });

    day = new Date(day);
    day.setUTCDate(day.getUTCDate() + between(random, 3, 6));
  }

  return releases;
}

// ---------------------------------------------------------------------------
// Build the rows
// ---------------------------------------------------------------------------

const packages = PACKAGES.map(([namespace, name, type, description, keywords, category, popularity]) => ({
  namespace,
  name,
  type,
  description,
  keywords,
  category,
  popularity,
  normalizedNamespace: normalize(namespace),
  normalizedName: normalize(name),
}));

if (packages.length !== 100) {
  throw new Error(`expected 100 packages, found ${packages.length}`);
}

const namespaceAuthor = new Map(NAMESPACES.map((entry) => [entry.name, entry.author]));

const versionRows = [];
const authorRows = [];
const keywordRows = [];
const dependencyRows = [];
const readmeRows = [];
const downloadRows = [];

let weightTotal = 0;
const weighted = [];

for (const pkg of packages) {
  const random = rng(hash(`${pkg.namespace}/${pkg.name}`));
  const releases = versionsFor(random, pkg.popularity);
  const body = readme(random, pkg);
  const license = pick(random, LICENSES);
  const repository = `https://github.com/${pkg.normalizedNamespace}/${pkg.normalizedName}`;
  const homepage = random() < 0.6 ? `https://rux-lang.dev/packages/${pkg.normalizedNamespace}/${pkg.normalizedName}` : null;

  readmeRows.push({ pkg, body });

  releases.forEach((release, index) => {
    const storageKey = `local-seed/${pkg.normalizedNamespace}/${pkg.normalizedName}/${release.version}.ruxpkg`;
    const artifactFileCount = between(random, 4, 220);
    const sourceFileCount = Math.max(1, Math.floor(artifactFileCount * (0.4 + random() * 0.5)));
    const artifactSize = between(random, 6_000, 900_000);

    const manifest = {
      manifest: { version: 1, min_rux: MIN_RUX },
      package: {
        namespace: pkg.namespace,
        name: pkg.name,
        version: release.version,
        type: pkg.type,
        description: pkg.description,
        authors: [namespaceAuthor.get(pkg.namespace)],
        keywords: pkg.keywords,
        license,
        repository,
        ...(homepage ? { homepage } : {}),
        readme_file: "README.md",
      },
      dependencies: {},
    };

    // Dependencies point at packages earlier in the curated list, which keeps
    // the graph acyclic and every target guaranteed to exist.
    const candidates = packages.filter(
      (other) => other !== pkg && other.popularity > pkg.popularity && other.namespace !== pkg.namespace,
    );
    const dependencyCount = candidates.length === 0 ? 0 : between(random, 0, Math.min(3, candidates.length));
    const used = new Set();
    for (let n = 0; n < dependencyCount; n += 1) {
      const target = pick(random, candidates);
      if (used.has(target.name)) continue;
      used.add(target.name);
      const range = pick(random, ["^1.0", "^1.1", "^2.0", ">=1.0, <2.0"]);
      manifest.dependencies[target.name] = { namespace: target.namespace, version: range };
      dependencyRows.push({
        namespace: pkg.normalizedNamespace,
        name: pkg.normalizedName,
        version: release.version,
        alias: target.name,
        targetNamespace: target.namespace,
        targetPackage: target.name,
        range,
      });
    }

    versionRows.push({
      pkg,
      release,
      license,
      repository,
      homepage,
      storageKey,
      artifactSize,
      artifactFileCount,
      sourceFileCount,
      sourceLineCount: sourceFileCount * between(random, 40, 400),
      artifactExpandedBytes: Math.min(10_485_760, artifactSize * between(random, 2, 5)),
      sha256: sha256Hex(storageKey),
      manifest: JSON.stringify(manifest),
    });

    authorRows.push({
      namespace: pkg.normalizedNamespace,
      name: pkg.normalizedName,
      version: release.version,
      ordinal: 0,
      author: namespaceAuthor.get(pkg.namespace),
    });
    if (random() < 0.25) {
      authorRows.push({
        namespace: pkg.normalizedNamespace,
        name: pkg.normalizedName,
        version: release.version,
        ordinal: 1,
        author: pick(random, ["Casey Example", "Robin Fields", "Alex Moreau", "Sam Okafor", "Jules Navarro"]),
      });
    }

    pkg.keywords.forEach((keyword, ordinal) => {
      keywordRows.push({
        namespace: pkg.normalizedNamespace,
        name: pkg.normalizedName,
        version: release.version,
        ordinal,
        keyword,
      });
    });

    // Newer releases carry most of the traffic, and yanked ones almost none.
    const recency = (index + 1) / releases.length;
    const weight = release.yanked ? 0.02 : pkg.popularity * recency * recency;
    weightTotal += weight;
    weighted.push({ storageKey, weight });
  });
}

for (const entry of weighted) {
  const count = Math.round((entry.weight / weightTotal) * TOTAL_DOWNLOAD_EVENTS);
  if (count > 0) downloadRows.push({ storageKey: entry.storageKey, count });
}

for (const row of versionRows) {
  const lifecycleEnd = new Date(row.release.publishedAt);
  if (row.release.yanked) lifecycleEnd.setUTCDate(lifecycleEnd.getUTCDate() + YANK_DELAY_DAYS);
  if (lifecycleEnd >= LOCAL_CATALOG_CUTOFF) {
    throw new Error(
      `${row.pkg.namespace}/${row.pkg.name}@${row.release.version} reaches ${lifecycleEnd.toISOString()}, ` +
        `after the local catalog cutoff ${LOCAL_CATALOG_CUTOFF.toISOString()}`,
    );
  }
}

// ---------------------------------------------------------------------------
// Emit
// ---------------------------------------------------------------------------

const out = [];
const push = (line = "") => out.push(line);

push("-- Generated by tools/catalog/generate-catalog.mjs — do not edit by hand.");
push("-- Regenerate with: node tools/catalog/generate-catalog.mjs");
push("--");
push(`-- ${packages.length} packages, ${versionRows.length} versions, ${keywordRows.length} keywords,`);
push(`-- ${dependencyRows.length} dependencies, ~${TOTAL_DOWNLOAD_EVENTS.toLocaleString("en-US")} download events.`);
push("");
push("BEGIN;");
push("");
push("-- Keep concurrent invocations from interleaving their natural-key lookups.");
push("SELECT pg_advisory_xact_lock(2026080102050);");
push("");

push("INSERT INTO namespaces (display_name, created_at, updated_at)");
push("VALUES");
push(
  NAMESPACES.map(
    (entry) => `    (${quote(entry.name)}, '${entry.createdAt} 00:00:00+00', '${entry.createdAt} 00:00:00+00')`,
  ).join(",\n"),
);
push("ON CONFLICT DO NOTHING;");
push("");

push("WITH seed_packages (namespace_name, package_name, created_at) AS (");
push("    VALUES");
push(
  packages
    .map((pkg, index) => {
      const created = new Date(Date.UTC(2026, 0, 5));
      created.setUTCDate(created.getUTCDate() + (index % 60));
      return `        (${quote(pkg.normalizedNamespace)}, ${quote(pkg.name)}, '${isoDay(created)} 01:00:00+00'::timestamptz)`;
    })
    .join(",\n"),
);
push(")");
push("INSERT INTO packages (namespace_id, display_name, created_at)");
push("SELECT namespaces.id, seed_packages.package_name, seed_packages.created_at");
push("FROM seed_packages");
push("JOIN namespaces ON namespaces.normalized_name = seed_packages.namespace_name");
push("ON CONFLICT DO NOTHING;");
push("");

push("-- READMEs are keyed by package so the text is emitted once rather than");
push("-- repeated across every release of the same package.");
push("WITH seed_readmes (namespace_name, package_name, readme_file_text) AS (");
push("    VALUES");
push(
  readmeRows
    .map(
      ({ pkg, body }) =>
        `        (${quote(pkg.normalizedNamespace)}, ${quote(pkg.normalizedName)}, ${dollar(body, "readme")})`,
    )
    .join(",\n"),
);
push("),");
push("seed_versions (");
push(
  [
    "namespace_name",
    "package_name",
    "version",
    "major",
    "minor",
    "patch",
    "prerelease",
    "build_metadata",
    "min_rux",
    "package_type",
    "description",
    "repository_url",
    "homepage_url",
    "readme_file_path",
    "license_expression",
    "normalized_manifest",
    "artifact_sha256",
    "artifact_size",
    "storage_key",
    "artifact_file_count",
    "artifact_expanded_bytes",
    "source_file_count",
    "source_line_count",
    "published_at",
    "yanked_at",
  ]
    .map((column) => `    ${column}`)
    .join(",\n"),
);
push(") AS (");
push("    VALUES");
push(
  versionRows
    .map((row) => {
      const published = row.release.publishedAt.toISOString().replace("T", " ").slice(0, 19);
      const yanked = row.release.yanked
        ? `'${published}'::timestamptz + interval '${YANK_DELAY_DAYS} days'`
        : "NULL::timestamptz";
      return `        (${[
        quote(row.pkg.normalizedNamespace),
        quote(row.pkg.normalizedName),
        quote(row.release.version),
        row.release.major,
        row.release.minor,
        row.release.patch,
        nullable(row.release.prerelease),
        nullable(row.release.buildMetadata),
        quote(MIN_RUX),
        quote(row.pkg.type),
        quote(row.pkg.description),
        quote(row.repository),
        nullable(row.homepage),
        quote("README.md"),
        quote(row.license),
        `${dollar(row.manifest, "json")}::jsonb`,
        quote(row.sha256),
        row.artifactSize,
        quote(row.storageKey),
        row.artifactFileCount,
        row.artifactExpandedBytes,
        row.sourceFileCount,
        row.sourceLineCount,
        `'${published}+00'::timestamptz`,
        yanked,
      ].join(", ")})`;
    })
    .join(",\n"),
);
push(")");
push("INSERT INTO package_versions (");
push(
  [
    "package_id",
    "version",
    "major",
    "minor",
    "patch",
    "prerelease",
    "build_metadata",
    "manifest_schema_version",
    "min_rux",
    "package_type",
    "description",
    "repository_url",
    "homepage_url",
    "readme_file_path",
    "readme_file_text",
    "license_expression",
    "normalized_manifest",
    "artifact_sha256",
    "artifact_size",
    "storage_key",
    "artifact_file_count",
    "artifact_expanded_bytes",
    "source_file_count",
    "source_line_count",
    "published_at",
    "yanked_at",
  ]
    .map((column) => `    ${column}`)
    .join(",\n"),
);
push(")");
push("SELECT");
push(
  [
    "packages.id",
    "seed_versions.version",
    "seed_versions.major",
    "seed_versions.minor",
    "seed_versions.patch",
    "seed_versions.prerelease",
    "seed_versions.build_metadata",
    "1",
    "seed_versions.min_rux",
    "seed_versions.package_type",
    "seed_versions.description",
    "seed_versions.repository_url",
    "seed_versions.homepage_url",
    "seed_versions.readme_file_path",
    "seed_readmes.readme_file_text",
    "seed_versions.license_expression",
    "seed_versions.normalized_manifest",
    "decode(seed_versions.artifact_sha256, 'hex')",
    "seed_versions.artifact_size",
    "seed_versions.storage_key",
    "seed_versions.artifact_file_count",
    "seed_versions.artifact_expanded_bytes",
    "seed_versions.source_file_count",
    "seed_versions.source_line_count",
    "seed_versions.published_at",
    "seed_versions.yanked_at",
  ]
    .map((column) => `    ${column}`)
    .join(",\n"),
);
push("FROM seed_versions");
push("JOIN seed_readmes");
push("    ON seed_readmes.namespace_name = seed_versions.namespace_name");
push("    AND seed_readmes.package_name = seed_versions.package_name");
push("JOIN namespaces ON namespaces.normalized_name = seed_versions.namespace_name");
push("JOIN packages");
push("    ON packages.namespace_id = namespaces.id");
push("    AND packages.normalized_name = seed_versions.package_name");
push("ON CONFLICT DO NOTHING;");
push("");

function joinedInsert(cteName, columns, rows, target, select) {
  push(`WITH ${cteName} (${columns.join(", ")}) AS (`);
  push("    VALUES");
  push(rows.join(",\n"));
  push(")");
  push(target);
  push(select);
  push(`FROM ${cteName}`);
  push(`JOIN namespaces ON namespaces.normalized_name = ${cteName}.namespace_name`);
  push("JOIN packages");
  push("    ON packages.namespace_id = namespaces.id");
  push(`    AND packages.normalized_name = ${cteName}.package_name`);
  push("JOIN package_versions");
  push("    ON package_versions.package_id = packages.id");
  push(`    AND package_versions.version = ${cteName}.version`);
  push("ON CONFLICT DO NOTHING;");
  push("");
}

joinedInsert(
  "seed_authors",
  ["namespace_name", "package_name", "version", "ordinal", "author"],
  authorRows.map(
    (row) =>
      `        (${quote(row.namespace)}, ${quote(row.name)}, ${quote(row.version)}, ${row.ordinal}, ${quote(row.author)})`,
  ),
  "INSERT INTO package_version_authors (package_version_id, ordinal, author)",
  "SELECT package_versions.id, seed_authors.ordinal, seed_authors.author",
);

joinedInsert(
  "seed_keywords",
  ["namespace_name", "package_name", "version", "ordinal", "keyword"],
  keywordRows.map(
    (row) =>
      `        (${quote(row.namespace)}, ${quote(row.name)}, ${quote(row.version)}, ${row.ordinal}, ${quote(row.keyword)})`,
  ),
  "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)",
  "SELECT package_versions.id, seed_keywords.ordinal, seed_keywords.keyword",
);

joinedInsert(
  "seed_dependencies",
  ["namespace_name", "package_name", "version", "alias", "target_namespace", "target_package", "version_range"],
  dependencyRows.map(
    (row) =>
      `        (${quote(row.namespace)}, ${quote(row.name)}, ${quote(row.version)}, ${quote(row.alias)}, ${quote(row.targetNamespace)}, ${quote(row.targetPackage)}, ${quote(row.range)})`,
  ),
  `INSERT INTO dependencies (
    package_version_id,
    display_alias,
    target_namespace_display_name,
    target_package_display_name,
    version_range
)`,
  `SELECT
    package_versions.id,
    seed_dependencies.alias,
    seed_dependencies.target_namespace,
    seed_dependencies.target_package,
    seed_dependencies.version_range`,
);

push("-- Download history. Timestamps are relative to now() so the 30-day");
push("-- highlights window always has data, however long after generation this");
push("-- seed is loaded. The exponent biases events toward the recent past.");
push("--");
push("-- download_events has no natural key to conflict on, so re-running the");
push("-- seed would otherwise double every count. The NOT EXISTS guard keeps it");
push("-- idempotent: a version that already has history is left alone.");
push("WITH seed_downloads (storage_key, event_count) AS (");
push("    VALUES");
push(downloadRows.map((row) => `        (${quote(row.storageKey)}, ${row.count})`).join(",\n"));
push(")");
push("INSERT INTO download_events (package_version_id, occurred_at)");
push("SELECT");
push("    package_versions.id,");
push(`    now() - (power(random(), 2.0) * interval '${DOWNLOAD_WINDOW_DAYS} days')`);
push("FROM seed_downloads");
push("JOIN package_versions ON package_versions.storage_key = seed_downloads.storage_key");
push("CROSS JOIN LATERAL generate_series(1, seed_downloads.event_count)");
push("WHERE NOT EXISTS (");
push("    SELECT 1 FROM download_events");
push("    WHERE download_events.package_version_id = package_versions.id");
push(");");
push("");
push("COMMIT;");
push("");

writeFileSync(OUTPUT, out.join("\n"), "utf8");

const downloadTotal = downloadRows.reduce((sum, row) => sum + row.count, 0);
console.log(`wrote ${OUTPUT}`);
console.log(
  [
    `namespaces: ${NAMESPACES.length}`,
    `packages: ${packages.length}`,
    `versions: ${versionRows.length}`,
    `authors: ${authorRows.length}`,
    `keywords: ${keywordRows.length}`,
    `dependencies: ${dependencyRows.length}`,
    `download events: ${downloadTotal}`,
  ].join("\n"),
);
