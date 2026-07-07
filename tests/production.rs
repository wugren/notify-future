use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll, Waker};

use notify_future::Notify;
#[allow(deprecated)]
use notify_future::NotifyFuture;

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

#[allow(deprecated)]
#[test]
fn notify_future_ready_after_set_complete_on_production_cfg() {
    let mut notify_future = NotifyFuture::<u32>::new();
    let notifier = notify_future.clone();

    notifier.set_complete(1);

    let mut cx = Context::from_waker(noop_waker());
    let value = match Pin::new(&mut notify_future).poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("future remained pending after set_complete"),
    };

    assert_eq!(value, 1);
}

#[test]
fn notify_waiter_ready_after_notify_on_production_cfg() {
    let (notify, mut waiter) = Notify::<u32>::new();

    notify.notify(1);

    let mut cx = Context::from_waker(noop_waker());
    let value = match Pin::new(&mut waiter).poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("waiter remained pending after notify"),
    };

    assert_eq!(value, 1);
}

#[test]
fn ready_value_is_dropped_if_waiter_is_dropped_on_production_cfg() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (notify, waiter) = Notify::<DropCounter>::new();

    notify.notify(DropCounter {
        drops: drops.clone(),
    });
    drop(waiter);

    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn notify_after_waiter_drop_discards_result_on_production_cfg() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (notify, waiter) = Notify::<DropCounter>::new();

    drop(waiter);
    assert!(notify.is_canceled());

    notify.notify(DropCounter {
        drops: drops.clone(),
    });

    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[allow(deprecated)]
#[test]
fn repeated_notify_future_set_complete_drops_losing_input_on_production_cfg() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut notify_future = NotifyFuture::<DropCounter>::new();
    let notifier = notify_future.clone();

    notifier.set_complete(DropCounter {
        drops: drops.clone(),
    });
    notifier.set_complete(DropCounter {
        drops: drops.clone(),
    });

    let mut cx = Context::from_waker(noop_waker());
    let value = match Pin::new(&mut notify_future).poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("future remained pending after set_complete"),
    };

    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(value);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}
