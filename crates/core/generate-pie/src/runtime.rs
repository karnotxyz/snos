use std::future::Future;

/// Drive mixed async/blocking execution off Tokio's scheduler threads. Blockifier
/// may join a synchronous worker that itself waits for RPC on this runtime.
pub(super) async fn run_blocking_future<F>(future: F) -> Result<F::Output, tokio::task::JoinError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(future)).await
}

#[cfg(test)]
mod tests {
    use super::run_blocking_future;
    use std::{sync::mpsc, time::Duration};

    async fn check_scheduler_progress() {
        let (response, pending_rpc) = mpsc::channel();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let execution = tokio::spawn(run_blocking_future(async move {
            started.send(()).unwrap();
            // Model Blockifier joining a worker that needs the async runtime to
            // complete an RPC. The timeout makes scheduler starvation fail boundedly.
            std::thread::spawn(move || pending_rpc.recv_timeout(Duration::from_secs(2))).join().unwrap()
        }));
        waiting.await.unwrap();
        tokio::spawn(async move { response.send(42).unwrap() }).await.unwrap();
        assert_eq!(execution.await.unwrap().unwrap().unwrap(), 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn current_thread_scheduler_services_rpc_during_blocking_execution() {
        check_scheduler_progress().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn single_worker_scheduler_services_rpc_during_blocking_execution() {
        check_scheduler_progress().await;
    }
}
