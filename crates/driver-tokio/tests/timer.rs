//! The timer driver.

use std::time::{Duration, SystemTime};

use caf_core::time::Mono;
use caf_driver_tokio::TimerDriver;

#[tokio::test]
async fn now_counts_from_the_drivers_start_and_reads_the_wall_clock() {
    let timer = TimerDriver::new();
    let first = timer.now();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = timer.now();
    assert!(second.mono.0 - first.mono.0 >= 20_000_000);
    assert!(first.mono.0 < 1_000_000_000, "{first:?}");
    let wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let wall = i64::try_from(wall).unwrap();
    assert!((wall - second.wall.0).abs() < 1000, "{second:?}");
}

#[tokio::test]
async fn it_wakes_at_the_deadline_once() {
    let mut timer = TimerDriver::new();
    let asked = timer.now();
    timer.wake_at(Some(asked.mono.after(Duration::from_millis(50))));
    tokio::time::timeout(Duration::from_secs(5), timer.woken())
        .await
        .unwrap();
    let woke = timer.now();
    assert!(
        woke.mono.0 - asked.mono.0 >= 50_000_000,
        "{asked:?} {woke:?}"
    );
    // The deadline went with it.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), timer.woken())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_deadline_passed_wakes_at_once() {
    let mut timer = TimerDriver::new();
    tokio::time::sleep(Duration::from_millis(5)).await;
    timer.wake_at(Some(Mono(0)));
    tokio::time::timeout(Duration::from_millis(100), timer.woken())
        .await
        .unwrap();
}

#[tokio::test]
async fn no_deadline_never_wakes() {
    let mut timer = TimerDriver::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), timer.woken())
            .await
            .is_err()
    );
    timer.wake_at(Some(Mono(u64::MAX)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), timer.woken())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_new_deadline_replaces_the_old() {
    let mut timer = TimerDriver::new();
    let now = timer.now();
    timer.wake_at(Some(now.mono.after(Duration::from_millis(20))));
    timer.wake_at(None);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), timer.woken())
            .await
            .is_err()
    );
    // Cancelled while waiting, it still has its deadline.
    timer.wake_at(Some(timer.now().mono.after(Duration::from_millis(150))));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), timer.woken())
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(5), timer.woken())
        .await
        .unwrap();
}
