//! 额度模块所有批量宿主回调共享同一并发上限，避免多个图片任务叠加查询并发。
use std::future::Future;

use gateway_plugin_sdk::{ErrorCode, PluginFault};
use tokio::sync::Semaphore;

const MAX_HOST_CALLS: usize = 4;
static HOST_CALLS: Semaphore = Semaphore::const_new(MAX_HOST_CALLS);

/// 调用方的原有超时包住此函数，因此排队等待也计入预算。
/// 名额由借用守卫管理；调用成功、失败或 future 被取消时都会自动归还。
pub(crate) async fn limited<T>(
    operation: impl Future<Output = Result<T, PluginFault>>,
) -> Result<T, PluginFault> {
    limited_by(&HOST_CALLS, operation).await
}

async fn limited_by<T>(
    semaphore: &Semaphore,
    operation: impl Future<Output = Result<T, PluginFault>>,
) -> Result<T, PluginFault> {
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "quota callback limiter unavailable"))?;
    operation.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[tokio::test]
    async fn independent_callers_share_one_callback_limit() {
        let semaphore = Arc::new(Semaphore::new(MAX_HOST_CALLS));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(MAX_HOST_CALLS));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let semaphore = Arc::clone(&semaphore);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                limited_by(&semaphore, async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    barrier.wait().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), MAX_HOST_CALLS);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(semaphore.available_permits(), MAX_HOST_CALLS);
    }

    #[tokio::test]
    async fn timeout_cancels_queued_and_active_calls_without_leaking_permits() {
        let semaphore = Semaphore::new(MAX_HOST_CALLS);
        let occupied = semaphore.acquire_many(MAX_HOST_CALLS as u32).await.unwrap();
        let invoked = AtomicBool::new(false);
        let queued = tokio::time::timeout(
            Duration::from_millis(1),
            limited_by(&semaphore, async {
                invoked.store(true, Ordering::SeqCst);
                Ok(())
            }),
        )
        .await;
        assert!(queued.is_err());
        assert!(!invoked.load(Ordering::SeqCst));
        drop(occupied);
        assert_eq!(semaphore.available_permits(), MAX_HOST_CALLS);

        let active = tokio::time::timeout(
            Duration::from_millis(1),
            limited_by(&semaphore, pending::<Result<(), PluginFault>>()),
        )
        .await;
        assert!(active.is_err());
        assert_eq!(semaphore.available_permits(), MAX_HOST_CALLS);

        let failure = limited_by::<()>(&semaphore, async {
            Err(PluginFault::new(ErrorCode::Fault, "test callback failed"))
        })
        .await;
        assert!(failure.is_err());
        assert_eq!(semaphore.available_permits(), MAX_HOST_CALLS);
    }
}
