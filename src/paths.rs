use url::Url;

pub(crate) fn canonical_package_path(namespace: &str, package: &str) -> String {
    canonical_path([namespace, package])
}

pub(crate) fn canonical_version_path(namespace: &str, package: &str, version: &str) -> String {
    canonical_path([namespace, package, version])
}

pub(crate) fn canonical_download_path(namespace: &str, package: &str, version: &str) -> String {
    canonical_path([namespace, package, version, "download"])
}

fn canonical_path<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let mut url = Url::parse("https://api.rux-lang.dev").expect("base URL should parse");
    url.path_segments_mut()
        .expect("base URL supports path segments")
        .extend(["v1", "packages"])
        .extend(segments);
    url.path().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_paths_preserve_supplied_normalized_segments_and_exact_versions() {
        assert_eq!(
            canonical_package_path("rux-tools", "example-pkg"),
            "/v1/packages/rux-tools/example-pkg"
        );
        assert_eq!(
            canonical_version_path("rux-tools", "example-pkg", "1.0.0+linux"),
            "/v1/packages/rux-tools/example-pkg/1.0.0+linux"
        );
        assert_eq!(
            canonical_download_path("rux-tools", "example-pkg", "1.0.0+linux"),
            "/v1/packages/rux-tools/example-pkg/1.0.0+linux/download"
        );
    }
}
