use std::future::Future;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;

#[cfg(test)]
use loom::cell::UnsafeCell;
#[cfg(test)]
use loom::sync::{atomic::AtomicU8, Arc};
#[cfg(not(test))]
use std::cell::UnsafeCell;
#[cfg(not(test))]
use std::sync::{atomic::AtomicU8, Arc};

const PENDING: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const TAKEN: u8 = 3;
const CANCELED: u8 = 4;

struct NotifyFutureState<RESULT> {
    state: AtomicU8,
    waker: AtomicWaker,
    result: UnsafeCell<MaybeUninit<RESULT>>,
}

// SAFETY: `state` serializes all writes, reads, and drops of `result`. Values
// cross thread boundaries only by being written by the notifier and read by the
// waiter, so `RESULT: Send` is sufficient.
unsafe impl<RESULT: Send> Send for NotifyFutureState<RESULT> {}
unsafe impl<RESULT: Send> Sync for NotifyFutureState<RESULT> {}

impl<RESULT> NotifyFutureState<RESULT> {
    pub fn new() -> Arc<NotifyFutureState<RESULT>> {
        Arc::new(NotifyFutureState {
            state: AtomicU8::new(PENDING),
            waker: AtomicWaker::new(),
            result: UnsafeCell::new(MaybeUninit::uninit()),
        })
    }

    pub fn set_complete(state: &Arc<NotifyFutureState<RESULT>>, result: RESULT) {
        state.set_complete_inner(result);
    }

    fn set_complete_inner(&self, result: RESULT) {
        if self
            .state
            .compare_exchange(PENDING, WRITING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        self.write_result(result);

        self.state.store(READY, Ordering::Release);
        self.waker.wake();
    }

    fn poll_result(&self, cx: &mut Context<'_>, already_taken_message: &str) -> Poll<RESULT> {
        match self.state.load(Ordering::Acquire) {
            READY => return Poll::Ready(self.take_result(already_taken_message)),
            TAKEN => panic!("{already_taken_message}"),
            _ => {}
        }

        self.waker.register(cx.waker());

        match self.state.load(Ordering::Acquire) {
            READY => Poll::Ready(self.take_result(already_taken_message)),
            TAKEN => panic!("{already_taken_message}"),
            _ => Poll::Pending,
        }
    }

    fn take_result(&self, already_taken_message: &str) -> RESULT {
        match self
            .state
            .compare_exchange(READY, TAKEN, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => self.read_result(),
            Err(TAKEN) => panic!("{already_taken_message}"),
            Err(_) => panic!("Notify result was not ready"),
        }
    }

    fn cancel(&self) {
        let _ = self
            .state
            .compare_exchange(PENDING, CANCELED, Ordering::AcqRel, Ordering::Acquire);
        let _ = self.waker.take();
    }

    pub fn is_canceled(&self) -> bool {
        self.state.load(Ordering::Acquire) == CANCELED
    }
}

#[cfg(not(test))]
impl<RESULT> NotifyFutureState<RESULT> {
    fn write_result(&self, result: RESULT) {
        // SAFETY: the PENDING -> WRITING transition gives this notifier unique
        // write access. Readers only read after observing READY.
        unsafe {
            self.result.get().write(MaybeUninit::new(result));
        }
    }

    fn read_result(&self) -> RESULT {
        // SAFETY: READY means the result was initialized, and the successful
        // READY -> TAKEN transition gives this caller unique ownership.
        unsafe { (*self.result.get()).as_ptr().read() }
    }

    fn drop_result(&mut self) {
        // SAFETY: READY means the result is initialized. `&mut self` means
        // no other Arc handles remain, so no other thread can take it now.
        unsafe {
            self.result.get_mut().assume_init_drop();
        }
    }
}

#[cfg(test)]
impl<RESULT> NotifyFutureState<RESULT> {
    fn write_result(&self, result: RESULT) {
        // SAFETY: the PENDING -> WRITING transition gives this notifier unique
        // write access. Readers only read after observing READY.
        self.result.with_mut(|ptr| unsafe {
            ptr.write(MaybeUninit::new(result));
        });
    }

    fn read_result(&self) -> RESULT {
        // SAFETY: READY means the result was initialized, and the successful
        // READY -> TAKEN transition gives this caller unique ownership.
        self.result.with(|ptr| unsafe { (*ptr).as_ptr().read() })
    }

    fn drop_result(&mut self) {
        // SAFETY: READY means the result is initialized. `&mut self` means
        // no other Arc handles remain, so no other thread can take it now.
        self.result.get_mut().with(|ptr| unsafe {
            (*ptr).assume_init_drop();
        });
    }
}

impl<RESULT> Drop for NotifyFutureState<RESULT> {
    fn drop(&mut self) {
        if self.state.load(Ordering::Acquire) == READY {
            self.drop_result();
        }
    }
}

#[deprecated(since = "0.2.1", note = "Please use Notify instead")]
pub struct NotifyFuture<RESULT> {
    state: Arc<NotifyFutureState<RESULT>>,
}

#[allow(deprecated)]
impl<RESULT> Clone for NotifyFuture<RESULT> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

#[allow(deprecated)]
impl<RESULT> NotifyFuture<RESULT> {
    pub fn new() -> Self {
        Self {
            state: NotifyFutureState::new(),
        }
    }

    pub fn set_complete(&self, result: RESULT) {
        NotifyFutureState::set_complete(&self.state, result);
    }
}

#[allow(deprecated)]
impl<RESULT> Default for NotifyFuture<RESULT> {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(deprecated)]
impl<RESULT> Future for NotifyFuture<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.state.poll_result(
            cx,
            "NotifyFuture was awaited by more than one task. Use Notify::new() instead",
        )
    }
}

pub struct Notify<RESULT> {
    state: Arc<NotifyFutureState<RESULT>>,
}

impl<RESULT> Notify<RESULT> {
    pub fn new() -> (Self, NotifyWaiter<RESULT>) {
        let state = NotifyFutureState::new();
        (
            Self {
                state: state.clone(),
            },
            NotifyWaiter::new(state),
        )
    }

    pub fn notify(self, result: RESULT) {
        NotifyFutureState::set_complete(&self.state, result);
    }

    pub fn is_canceled(&self) -> bool {
        self.state.is_canceled()
    }
}

pub struct NotifyWaiter<RESULT> {
    state: Arc<NotifyFutureState<RESULT>>,
}

impl<RESULT> NotifyWaiter<RESULT> {
    pub(crate) fn new(state: Arc<NotifyFutureState<RESULT>>) -> Self {
        Self { state }
    }
}

impl<RESULT> Drop for NotifyWaiter<RESULT> {
    fn drop(&mut self) {
        self.state.cancel();
    }
}

impl<RESULT> Future for NotifyWaiter<RESULT> {
    type Output = RESULT;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.state
            .poll_result(cx, "NotifyWaiter was polled after completion")
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod test {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{
        atomic::{AtomicUsize as StdAtomicUsize, Ordering},
        Arc as StdArc,
    };
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    use loom::sync::{atomic::AtomicUsize, Arc};
    use loom::thread;

    use crate::{Notify, NotifyFuture};

    struct DropCounter {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn noop_waker() -> &'static Waker {
        Waker::noop()
    }

    fn counting_waker(wakes: StdArc<StdAtomicUsize>) -> Waker {
        unsafe fn clone(data: *const ()) -> RawWaker {
            let wakes = StdArc::<StdAtomicUsize>::from_raw(data.cast::<StdAtomicUsize>());
            let cloned = wakes.clone();
            let _ = StdArc::into_raw(wakes);
            RawWaker::new(StdArc::into_raw(cloned).cast::<()>(), &VTABLE)
        }

        unsafe fn wake(data: *const ()) {
            let wakes = StdArc::<StdAtomicUsize>::from_raw(data.cast::<StdAtomicUsize>());
            wakes.fetch_add(1, Ordering::SeqCst);
        }

        unsafe fn wake_by_ref(data: *const ()) {
            let wakes = StdArc::<StdAtomicUsize>::from_raw(data.cast::<StdAtomicUsize>());
            wakes.fetch_add(1, Ordering::SeqCst);
            let _ = StdArc::into_raw(wakes);
        }

        unsafe fn drop(data: *const ()) {
            let _ = StdArc::<StdAtomicUsize>::from_raw(data.cast::<StdAtomicUsize>());
        }

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        let raw = RawWaker::new(StdArc::into_raw(wakes).cast::<()>(), &VTABLE);
        unsafe { Waker::from_raw(raw) }
    }

    #[test]
    fn notify_future_ready_after_set_complete() {
        loom::model(|| {
            let mut notify_future = NotifyFuture::<u32>::new();
            let notifier = notify_future.clone();

            notifier.set_complete(1);

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut notify_future).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("future remained pending after set_complete"),
            };

            assert_eq!(value, 1);
        });
    }

    #[test]
    fn notify_waiter_ready_after_notify() {
        loom::model(|| {
            let (notify, mut waiter) = Notify::<u32>::new();

            notify.notify(1);

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut waiter).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("waiter remained pending after notify"),
            };

            assert_eq!(value, 1);
        });
    }

    #[test]
    fn notify_waiter_drop_before_ready_is_canceled() {
        loom::model(|| {
            let (notify, waiter) = Notify::<u32>::new();
            drop(waiter);
            assert!(notify.is_canceled());
        });
    }

    #[test]
    fn notify_waiter_wakes_registered_waker() {
        loom::model(|| {
            let (notify, mut waiter) = Notify::<u32>::new();
            let wakes = StdArc::new(StdAtomicUsize::new(0));
            let waker = counting_waker(wakes.clone());
            let mut cx = Context::from_waker(&waker);

            assert!(matches!(Pin::new(&mut waiter).poll(&mut cx), Poll::Pending));

            notify.notify(1);

            assert_eq!(wakes.load(Ordering::SeqCst), 1);
            let value = match Pin::new(&mut waiter).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("waiter remained pending after notify"),
            };
            assert_eq!(value, 1);
        });
    }

    #[test]
    fn notify_after_waiter_drop_discards_result() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let (notify, waiter) = Notify::<DropCounter>::new();

            drop(waiter);
            assert!(notify.is_canceled());

            notify.notify(DropCounter {
                drops: drops.clone(),
            });

            assert_eq!(drops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn waiter_drop_after_ready_does_not_cancel() {
        loom::model(|| {
            let state = super::NotifyFutureState::new();
            let notify = Notify {
                state: state.clone(),
            };
            let waiter = super::NotifyWaiter::new(state.clone());

            notify.notify(1);
            drop(waiter);

            assert!(!state.is_canceled());
        });
    }

    #[test]
    fn repeated_set_complete_keeps_first_value() {
        loom::model(|| {
            let mut notify_future = NotifyFuture::<u32>::new();
            let notifier = notify_future.clone();

            notifier.set_complete(1);
            notifier.set_complete(2);

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut notify_future).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("future remained pending after set_complete"),
            };

            assert_eq!(value, 1);
        });
    }

    #[test]
    fn ready_value_is_dropped_if_waiter_is_dropped() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let (notify, waiter) = Notify::<DropCounter>::new();

            notify.notify(DropCounter {
                drops: drops.clone(),
            });
            drop(waiter);

            assert_eq!(drops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn taken_value_is_not_dropped_twice() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let (notify, mut waiter) = Notify::<DropCounter>::new();

            notify.notify(DropCounter {
                drops: drops.clone(),
            });

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut waiter).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("waiter remained pending after notify"),
            };

            assert_eq!(drops.load(Ordering::SeqCst), 0);
            drop(value);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn notify_racing_waiter_drop_drops_value_once() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let (notify, waiter) = Notify::<DropCounter>::new();

            let notify_thread = {
                let drops = drops.clone();
                thread::spawn(move || {
                    notify.notify(DropCounter { drops });
                })
            };

            let drop_thread = thread::spawn(move || {
                drop(waiter);
            });

            notify_thread.join().unwrap();
            drop_thread.join().unwrap();

            assert_eq!(drops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn poll_racing_notify_observes_one_ready_value() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let (notify, mut waiter) = Notify::<DropCounter>::new();

            let notify_thread = {
                let drops = drops.clone();
                thread::spawn(move || {
                    notify.notify(DropCounter { drops });
                })
            };

            let mut cx = Context::from_waker(noop_waker());

            match Pin::new(&mut waiter).poll(&mut cx) {
                Poll::Ready(value) => drop(value),
                Poll::Pending => {
                    notify_thread.join().unwrap();
                    let value = match Pin::new(&mut waiter).poll(&mut cx) {
                        Poll::Ready(value) => value,
                        Poll::Pending => panic!("waiter remained pending after notify"),
                    };
                    drop(value);
                    assert_eq!(drops.load(Ordering::SeqCst), 1);
                    return;
                }
            }

            notify_thread.join().unwrap();
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn repeated_notify_future_set_complete_drops_both_inputs_once() {
        loom::model(|| {
            let drops = Arc::new(AtomicUsize::new(0));
            let mut notify_future = NotifyFuture::<DropCounter>::new();
            let first_notifier = notify_future.clone();
            let second_notifier = notify_future.clone();

            let first_thread = {
                let drops = drops.clone();
                thread::spawn(move || {
                    first_notifier.set_complete(DropCounter { drops });
                })
            };

            let second_thread = {
                let drops = drops.clone();
                thread::spawn(move || {
                    second_notifier.set_complete(DropCounter { drops });
                })
            };

            first_thread.join().unwrap();
            second_thread.join().unwrap();

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut notify_future).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("future remained pending after set_complete"),
            };

            assert_eq!(drops.load(Ordering::SeqCst), 1);
            drop(value);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    #[should_panic(expected = "NotifyWaiter was polled after completion")]
    fn notify_waiter_panics_when_polled_after_completion() {
        loom::model(|| {
            let (notify, mut waiter) = Notify::<u32>::new();

            notify.notify(1);

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut waiter).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("waiter remained pending after notify"),
            };
            assert_eq!(value, 1);

            let _ = Pin::new(&mut waiter).poll(&mut cx);
        });
    }

    #[test]
    #[should_panic(expected = "NotifyFuture was awaited by more than one task")]
    fn notify_future_panics_when_polled_after_result_taken() {
        loom::model(|| {
            let mut first_waiter = NotifyFuture::<u32>::new();
            let notifier = first_waiter.clone();
            let mut second_waiter = first_waiter.clone();

            notifier.set_complete(1);

            let mut cx = Context::from_waker(noop_waker());
            let value = match Pin::new(&mut first_waiter).poll(&mut cx) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("future remained pending after set_complete"),
            };
            assert_eq!(value, 1);

            let _ = Pin::new(&mut second_waiter).poll(&mut cx);
        });
    }
}
