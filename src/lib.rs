use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::pin::Pin;
use std::task::{Poll, Context, Waker};

struct NotifyFutureState<RESULT> {
    waker: Option<Waker>,
    result: Option<RESULT>,
    is_completed: bool,
    is_canceled: bool,
}

impl <RESULT> NotifyFutureState<RESULT> {
    pub fn new() -> Arc<Mutex<NotifyFutureState<RESULT>>> {
        Arc::new(Mutex::new(NotifyFutureState {
            waker: None,
            result: None,
            is_completed: false,
            is_canceled: false,
        }))
    }

    pub fn set_complete(state: &Arc<Mutex<NotifyFutureState<RESULT>>>, result: RESULT) {
        let waker = {
            let mut state = lock_state(state);
            if state.is_completed || state.is_canceled {
                return;
            }

            state.result = Some(result);
            state.is_completed = true;
            state.waker.take()
        };

        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn is_canceled(&self) -> bool {
        self.is_canceled
    }
}

fn lock_state<RESULT>(state: &Arc<Mutex<NotifyFutureState<RESULT>>>) -> MutexGuard<'_, NotifyFutureState<RESULT>> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[deprecated(
    since = "0.2.1",
    note = "Please use Notify instead"
)]
pub struct NotifyFuture<RESULT> {
    state:Arc<Mutex<NotifyFutureState<RESULT>>>
}

#[allow(deprecated)]
impl<RESULT> Clone for NotifyFuture<RESULT> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone()
        }
    }
}

#[allow(deprecated)]
impl <RESULT> NotifyFuture<RESULT> {
    pub fn new() -> Self {
        Self{
            state: NotifyFutureState::new()
        }
    }

    pub fn set_complete(&self, result: RESULT) {
        NotifyFutureState::set_complete(&self.state, result);
    }
}

#[allow(deprecated)]
impl <RESULT> Future for NotifyFuture<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = lock_state(&self.state);
        if state.is_completed {
            if let Some(result) = state.result.take() {
                return Poll::Ready(result);
            }

            panic!("NotifyFuture was awaited by more than one task. Use Notify::new() instead");
        }

        if state.waker.is_none() || !state.waker.as_ref().unwrap().will_wake(cx.waker()) {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

pub struct Notify<RESULT> {
    state: Arc<Mutex<NotifyFutureState<RESULT>>>
}

impl<RESULT> Notify<RESULT> {
    pub fn new() -> (Self, NotifyWaiter<RESULT>) {
        let state = NotifyFutureState::new();
        (Self {
            state: state.clone()
        }, NotifyWaiter::new(state))
    }

    pub fn notify(self, result: RESULT) {
        NotifyFutureState::set_complete(&self.state, result);
    }

    pub fn is_canceled(&self) -> bool {
        lock_state(&self.state).is_canceled()
    }
}

pub struct NotifyWaiter<RESULT> {
    state: Arc<Mutex<NotifyFutureState<RESULT>>>
}

impl<RESULT> NotifyWaiter<RESULT> {
    pub(crate) fn new(state: Arc<Mutex<NotifyFutureState<RESULT>>>) -> Self {
        Self {
            state
        }
    }
}

impl<RESULT> Drop for NotifyWaiter<RESULT> {
    fn drop(&mut self) {
        let mut state = lock_state(&self.state);
        state.waker.take();
        if !state.is_completed {
            state.is_canceled = true;
        }
    }
}

impl <RESULT> Future for NotifyWaiter<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = lock_state(&self.state);
        if state.is_completed {
            return Poll::Ready(state.result.take().unwrap());
        }

        if state.waker.is_none() || !state.waker.as_ref().unwrap().will_wake(cx.waker()) {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod test {
    use std::time::Duration;
    use crate::{Notify, NotifyFuture};

    #[test]
    fn test() {
        async_std::task::block_on(async {
            let notify_future = NotifyFuture::<u32>::new();
            let tmp_future = notify_future.clone();
            async_std::task::spawn(async move {
                async_std::task::sleep(Duration::from_millis(2000)).await;
                tmp_future.set_complete(1);
            });
            let ret = notify_future.await;
            assert_eq!(ret, 1);
        });
    }

    #[test]
    fn test2() {
        async_std::task::block_on(async {
            let (notify, waiter) = Notify::<u32>::new();
            async_std::task::spawn(async move {
                async_std::task::sleep(Duration::from_millis(2000)).await;
                notify.notify(1);
            });
            let ret = waiter.await;
            assert_eq!(ret, 1);
        });
    }

    #[test]
    fn notify_waiter_drop_before_ready_is_canceled() {
        let (notify, waiter) = Notify::<u32>::new();
        drop(waiter);
        assert!(notify.is_canceled());
    }

    #[test]
    fn repeated_set_complete_keeps_first_value() {
        async_std::task::block_on(async {
            let notify_future = NotifyFuture::<u32>::new();
            let notifier = notify_future.clone();

            notifier.set_complete(1);
            notifier.set_complete(2);

            let ret = notify_future.await;
            assert_eq!(ret, 1);
        });
    }
}
