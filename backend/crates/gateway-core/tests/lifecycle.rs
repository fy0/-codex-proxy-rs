use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::{FutureExt as _, pin_mut};
use gateway_core::lifecycle::CancellationToken;

#[test]
fn cancellation_token_should_wake_current_state() {
    let token = CancellationToken::new();
    token.cancel();

    assert!(token.is_cancelled());
}

#[test]
fn child_cancellation_inherits_parent_without_cancelling_parent() {
    futures::executor::block_on(async {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        child.cancel();

        child.cancelled().await;
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    });
}

#[test]
fn parent_cancellation_wakes_nested_descendants() {
    futures::executor::block_on(async {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let grandchild = child.child_token();
        let waiting = grandchild.cancelled();
        pin_mut!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());

        parent.cancel();
        waiting.await;
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    });
}
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};

#[derive(Default)]
struct LifecycleState {
    draining: AtomicBool,
    active: AtomicUsize,
}

struct TestGuard {
    state: Arc<LifecycleState>,
}

impl ConnectionGuard for TestGuard {}

impl Drop for TestGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct TestLifecycle {
    state: Arc<LifecycleState>,
    cancellation: CancellationToken,
}

impl TestLifecycle {
    fn new() -> Self {
        Self {
            state: Arc::new(LifecycleState::default()),
            cancellation: CancellationToken::new(),
        }
    }

    fn begin_draining(&self) {
        self.state.draining.store(true, Ordering::Release);
        self.cancellation.cancel();
    }

    fn active(&self) -> usize {
        self.state.active.load(Ordering::Acquire)
    }
}

impl ConnectionLifecycle for TestLifecycle {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        if self.state.draining.load(Ordering::Acquire) {
            return Err(ConnectionDraining);
        }
        self.state.active.fetch_add(1, Ordering::AcqRel);
        if self.state.draining.load(Ordering::Acquire) {
            self.state.active.fetch_sub(1, Ordering::AcqRel);
            return Err(ConnectionDraining);
        }
        Ok(Box::new(TestGuard {
            state: Arc::clone(&self.state),
        }))
    }

    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn is_draining(&self) -> bool {
        self.state.draining.load(Ordering::Acquire)
    }
}

#[test]
fn connection_guard_drop_releases_active_registration() {
    let lifecycle = TestLifecycle::new();
    let guard = lifecycle.try_register().expect("registration before drain");
    drop(guard);

    assert_eq!(lifecycle.active(), 0);
}

#[test]
fn connection_registration_rejects_after_drain_linearization() {
    let lifecycle = TestLifecycle::new();
    lifecycle.begin_draining();

    assert!(matches!(lifecycle.try_register(), Err(ConnectionDraining)));
}

#[test]
fn connection_lifecycle_contract_is_object_safe() {
    let lifecycle = TestLifecycle::new();
    let object: &dyn ConnectionLifecycle = &lifecycle;

    assert!(!object.is_draining());
}
