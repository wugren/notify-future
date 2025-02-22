use std::future::Future;
use std::sync::{Mutex, Arc};
use std::pin::Pin;
use std::task::{Poll, Context, Waker};

struct NotifyFutureState<RESULT> {
    waker: Option<Waker>,
    result: Option<RESULT>,
}

impl <RESULT> NotifyFutureState<RESULT> {
    pub fn new() -> Arc<Mutex<NotifyFutureState<RESULT>>> {
        Arc::new(Mutex::new(NotifyFutureState {
            waker: None,
            result: None
        }))
    }

    pub fn set_complete(state: &Arc<Mutex<NotifyFutureState<RESULT>>>, result: RESULT) {
        let mut state = state.lock().unwrap();
        state.result = Some(result);
        if state.waker.is_some() {
            state.waker.take().unwrap().wake();
        }
    }
}

pub struct NotifyFuture<RESULT> {
    state:Arc<Mutex<NotifyFutureState<RESULT>>>
}

impl<RESULT> Clone for NotifyFuture<RESULT> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone()
        }
    }
}

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

impl <RESULT> Future for NotifyFuture<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        if state.result.is_some() {
            return Poll::Ready(state.result.take().unwrap());
        }

        if state.waker.is_none() {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

pub struct Notify<RESULT> {
    state: Arc<Mutex<NotifyFutureState<RESULT>>>
}

impl<RESULT> Notify<RESULT> {
    fn new() -> (Self, NotifyWaiter<RESULT>) {
        let state = NotifyFutureState::new();
        (Self {
            state: state.clone()
        }, NotifyWaiter::new(state))
    }

    pub fn notify(self, result: RESULT) {
        NotifyFutureState::set_complete(&self.state, result);
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

impl <RESULT> Future for NotifyWaiter<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        if state.result.is_some() {
            return Poll::Ready(state.result.take().unwrap());
        }

        if state.waker.is_none() {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use crate::{Notify, NotifyFuture};

    #[test]
    fn test() {
        async_std::task::block_on(async {
            let notify_future = NotifyFuture::<u32>::new();
            let tmp_future = notify_future.clone();
            async_std::task::spawn(async move {
                async_std::task::sleep(Duration::from_secs(3)).await;
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
                async_std::task::sleep(Duration::from_secs(3)).await;
                notify.notify(1);
            });
            let ret = waiter.await;
            assert_eq!(ret, 1);
        });
    }
}
