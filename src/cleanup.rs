use std::sync::Arc;
use std::time::{Duration, Instant};

use rux_application::{ObjectVersionCursor, OrphanCleanupService};
use tokio::sync::watch;
use tracing::{Instrument, info, info_span, warn};

pub async fn run(
    service: Arc<OrphanCleanupService>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut cursor: Option<ObjectVersionCursor> = None;

    loop {
        if *shutdown.borrow() {
            return;
        }
        let started = Instant::now();
        let span = info_span!("orphan_cleanup_sweep");
        let log_span = span.clone();
        let sweep = service.sweep(cursor.as_ref()).instrument(span);
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            outcome = sweep => {
                let _span_guard = log_span.enter();
                match outcome {
                    Ok(result) => {
                        info!(
                            scanned = result.scanned,
                            recognizable = result.recognizable,
                            old = result.old,
                            referenced = result.referenced,
                            delete_attempted = result.delete_attempted,
                            deleted = result.deleted,
                            delete_failed = result.delete_failed,
                            has_next_cursor = result.next_cursor.is_some(),
                            duration_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "orphan cleanup sweep completed"
                        );
                        cursor = result.next_cursor;
                    }
                    Err(error) => {
                        warn!(
                            kind = ?error.kind(),
                            duration_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "orphan cleanup sweep failed"
                        );
                    }
                }
            }
        }

        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use rux_application::{
        ArtifactReferenceReader, Clock, ObjectVersionPage, ObjectVersionStorageError,
        OrphanCleanupPolicy, RepositoryError, VersionedArtifactStorage,
    };
    use time::{Duration as TimeDuration, OffsetDateTime};
    use tokio::sync::Notify;

    use super::*;

    struct CountingStorage {
        calls: AtomicUsize,
        called: Notify,
    }

    #[async_trait]
    impl VersionedArtifactStorage for CountingStorage {
        async fn list_object_versions(
            &self,
            _cursor: Option<&ObjectVersionCursor>,
            _limit: u16,
        ) -> Result<ObjectVersionPage, ObjectVersionStorageError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.called.notify_one();
            Ok(ObjectVersionPage {
                versions: Vec::new(),
                next_cursor: None,
            })
        }

        async fn delete_object_version(
            &self,
            _key: &str,
            _version_id: &str,
        ) -> Result<(), ObjectVersionStorageError> {
            unreachable!("empty pages do not delete")
        }
    }

    struct EmptyReferences;

    #[async_trait]
    impl ArtifactReferenceReader for EmptyReferences {
        async fn referenced_storage_keys(
            &self,
            _keys: &[String],
        ) -> Result<Vec<String>, RepositoryError> {
            unreachable!("empty pages do not query references")
        }
    }

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH
        }
    }

    #[tokio::test]
    async fn worker_runs_immediately_repeats_and_stops_on_shutdown() {
        let storage = Arc::new(CountingStorage {
            calls: AtomicUsize::new(0),
            called: Notify::new(),
        });
        let policy = OrphanCleanupPolicy::new(TimeDuration::hours(24), 1_000, 100)
            .expect("worker policy should be valid");
        let service = Arc::new(OrphanCleanupService::new(
            storage.clone(),
            Arc::new(EmptyReferences),
            Arc::new(FixedClock),
            policy,
        ));
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);

        let worker = tokio::spawn(run(service, Duration::from_millis(1), shutdown_receiver));
        storage.called.notified().await;
        storage.called.notified().await;
        shutdown_sender
            .send(true)
            .expect("worker should retain the receiver");
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("worker should stop promptly")
            .expect("worker should not panic");

        assert!(storage.calls.load(Ordering::SeqCst) >= 2);
    }
}
