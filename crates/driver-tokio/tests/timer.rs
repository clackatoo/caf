//! The timer driver, on tokio's paused clock, so every time is exact.

use std::time::{Duration, SystemTime};

use caf_core::time::Mono;
use caf_driver_tokio::TimerDriver;

const MS: u64 = 1_000_000;

#[tokio::test(start_paused = true)]
async fn now_counts_from_the_drivers_start_on_tokios_clock() {
    let timer = TimerDriver::new();
    let first = timer.now();
    assert_eq!(first.mono, Mono(0));
    tokio::time::advance(Duration::from_millis(20)).await;
    assert_eq!(timer.now().mono, Mono(20 * MS));
}

#[tokio::test]
async fn the_wall_clock_is_the_systems() {
    let timer = TimerDriver::new();
    let wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let wall = i64::try_from(wall).unwrap();
    assert!((wall - timer.now().wall.0).abs() < 1000);
}

#[tokio::test(start_paused = true)]
async fn it_wakes_at_the_deadline_once() {
    let mut timer = TimerDriver::new();
    timer.wake_at(Some(Mono(50 * MS)));
    timer.woken().await;
    assert_eq!(timer.now().mono, Mono(50 * MS));
    // The deadline went with it.
    let again = tokio::time::timeout(Duration::from_secs(3600), timer.woken()).await;
    assert!(again.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_deadline_passed_wakes_at_once() {
    let mut timer = TimerDriver::new();
    tokio::time::advance(Duration::from_millis(5)).await;
    timer.wake_at(Some(Mono(0)));
    timer.woken().await;
    assert_eq!(timer.now().mono, Mono(5 * MS));
}

#[tokio::test(start_paused = true)]
async fn no_deadline_never_wakes() {
    let mut timer = TimerDriver::new();
    let woken = tokio::time::timeout(Duration::from_secs(3600), timer.woken()).await;
    assert!(woken.is_err());
    timer.wake_at(Some(Mono(u64::MAX)));
    let woken = tokio::time::timeout(Duration::from_secs(3600), timer.woken()).await;
    assert!(woken.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_new_deadline_replaces_the_old() {
    let mut timer = TimerDriver::new();
    timer.wake_at(Some(Mono(20 * MS)));
    timer.wake_at(None);
    let woken = tokio::time::timeout(Duration::from_millis(100), timer.woken()).await;
    assert!(woken.is_err());
    timer.wake_at(Some(Mono(250 * MS)));
    // Dropped while waiting, it keeps its deadline.
    let woken = tokio::time::timeout(Duration::from_millis(20), timer.woken()).await;
    assert!(woken.is_err());
    timer.woken().await;
    assert_eq!(timer.now().mono, Mono(250 * MS));
}
