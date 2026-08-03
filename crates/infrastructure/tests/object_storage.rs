use std::env;

use rux_application::{ArtifactSha256, ArtifactStorage, ArtifactUpload, VersionedArtifactStorage};
use rux_domain::{IdentitySegment, SemanticVersion};
use rux_infrastructure::{Infrastructure, InfrastructureConfig};
use sha2::{Digest, Sha256};

#[tokio::test]
#[ignore = "requires the configured S3-compatible package bucket"]
async fn configured_storage_accepts_a_verified_immutable_artifact() {
    let infrastructure = Infrastructure::new(InfrastructureConfig {
        database_url: required("RUX_DATABASE_URL"),
        storage_endpoint: required("RUX_STORAGE_ENDPOINT"),
        storage_bucket: required("RUX_STORAGE_BUCKET"),
        storage_access_key: required("RUX_STORAGE_ACCESS_KEY"),
        storage_secret_key: required("RUX_STORAGE_SECRET_KEY"),
        storage_region: required("RUX_STORAGE_REGION"),
        storage_force_path_style: required("RUX_STORAGE_FORCE_PATH_STYLE")
            .parse()
            .expect("path-style setting should be Boolean"),
    })
    .await
    .expect("infrastructure should configure");
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("minio-contract.ruxpkg");
    let bytes = b"Rux object-storage integration contract";
    std::fs::write(&path, bytes).expect("artifact should write");
    let checksum = ArtifactSha256::new(Sha256::digest(bytes).into());

    let stored = infrastructure
        .artifact_storage()
        .store(ArtifactUpload {
            path,
            namespace: IdentitySegment::new("Integration").expect("valid namespace"),
            package: IdentitySegment::new("Storage_Contract").expect("valid package"),
            version: SemanticVersion::new("1.0.0").expect("valid version"),
            sha256: checksum,
            byte_size: bytes.len() as u64,
        })
        .await
        .expect("verified upload should succeed");

    assert_eq!(stored.sha256, checksum);
    assert_eq!(stored.byte_size, bytes.len() as u64);
    assert!(
        stored
            .storage_key
            .starts_with("packages/integration/storage-contract/1.0.0/")
    );

    let page = infrastructure
        .artifact_storage()
        .list_object_versions(None, 1_000)
        .await
        .expect("configured storage should list object versions");
    let version_id = page
        .versions
        .iter()
        .find(|version| version.key.as_deref() == Some(&stored.storage_key))
        .and_then(|version| version.version_id.as_deref())
        .expect("stored artifact should have an exact version ID")
        .to_owned();
    infrastructure
        .artifact_storage()
        .delete_object_version(&stored.storage_key, &version_id)
        .await
        .expect("configured storage should delete one exact version");
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be configured"))
}
