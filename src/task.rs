//! A process-owning task always carries its cancellation and join obligation.
use std::future::Future;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

pub(crate) struct OwnedTask<T: Send + 'static> {
    cancel: CancellationToken,
    handle: Option<JoinHandle<T>>,
}
impl<T: Send + 'static> OwnedTask<T> {
    pub fn spawn<F, Fut>(cancel: CancellationToken, run: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        Self {
            handle: Some(tokio::spawn(run(cancel.clone()))),
            cancel,
        }
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub async fn join(mut self) -> Result<T, JoinError> {
        let result = self.handle.as_mut().expect("owned task").await;
        self.handle.take();
        result
    }
}
impl<T: Send + 'static> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            // Abnormal owner exit still lets async termination and reaping complete.
            // Normal paths explicitly cancel and join before releasing the endpoint.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = handle.await;
                });
            } else {
                handle.abort();
            }
        }
    }
}
