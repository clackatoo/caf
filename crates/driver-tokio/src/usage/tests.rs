use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use caf_core::usage::{PriceLayer, UsageQuery};

use super::*;

/// Long enough for anything a test waits for, on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(60);

/// A folder of its own for each test, under `TMPDIR`.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("caf-usage-driver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Turn `turn` of a chat, ended now, which spent `output` tokens.
fn line(turn: u64, output: u64) -> Box<LedgerLine> {
    let line = serde_json::json!({
        "at": wall_now().0,
        "app": "caf-test",
        "chat": 1000,
        "turn": turn,
        "agent": "fake",
        "profile": null,
        "usage": {
            "models": [{"model": "claude-test-1", "tokens": {
                "input": 10, "cache_read": 0, "cache_write": 0, "output": output, "thinking": null,
            }}],
            "cost": null,
            "fast": false,
        },
    });
    Box::new(serde_json::from_value(line).unwrap())
}

/// LiteLLM's table, as fetched, pricing one model.
fn litellm(model: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        model: {
            "litellm_provider": "anthropic",
            "mode": "chat",
            "input_cost_per_token": 0.000_001,
            "output_cost_per_token": 0.000_005,
        },
    }))
    .unwrap()
}

/// A fetcher giving `json`, counting how often it's called.
fn counting(json: Result<Vec<u8>, String>) -> (Fetcher, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    let fetcher = Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        json.clone()
    });
    (fetcher, count)
}

/// The driver's next report.
async fn next(driver: &mut UsageDriver) -> Report {
    (tokio::time::timeout(PATIENCE, driver.next()).await).expect("a report")
}

/// The driver's next prices.
async fn prices(driver: &mut UsageDriver) -> PriceTable {
    match next(driver).await {
        Report::Prices(prices) => prices,
        _ => panic!("prices"),
    }
}

/// How many turns the ledger in `folder` has this month.
fn turns_in(folder: &Path) -> u64 {
    let now = wall_now();
    let query = UsageQuery::new(Wall(now.0 - 3_600_000), Wall(now.0 + 3_600_000), "UTC");
    let summary = caf_usage::summarise(folder, &query, &PriceTable::default()).unwrap();
    assert!(summary.unreadable.is_empty(), "{:?}", summary.unreadable);
    summary.buckets.iter().map(|bucket| bucket.turns).sum()
}

#[tokio::test]
async fn with_no_ledger_the_prices_are_the_bundled_snapshot_and_lines_are_dropped() {
    let mut driver = UsageDriver::start(None, "caf-test".into(), None, CHECK_EVERY);
    assert_eq!(prices(&mut driver).await, prices::bundled());
    driver.record(line(1, 5));
    let (reply, answer) = oneshot::channel();
    driver.set_price(
        "claude-test-1".into(),
        Some(ModelPrice::new(1.0, 2.0)),
        reply,
    );
    assert_eq!(answer.await.unwrap(), Err(Error::NoLedger));
    driver.finish().await;
}

#[tokio::test]
async fn the_prices_are_read_from_the_ledgers_folder_as_it_starts() {
    let dir = Dir::new("start");
    prices::set_own(&dir.0, "claude-test-1", Some(ModelPrice::new(3.0, 4.0))).unwrap();
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    let table = prices(&mut driver).await;
    assert_eq!(table, prices::load(&dir.0));
    let own = table.find("claude-test-1").expect("the user's own");
    assert_eq!(own.layer, PriceLayer::Own);
    driver.finish().await;
}

#[tokio::test]
async fn each_line_reaches_the_ledger_by_the_time_it_finishes() {
    let dir = Dir::new("lines");
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    for turn in 1..=3 {
        driver.record(line(turn, 5));
    }
    driver.finish().await;
    assert_eq!(turns_in(&dir.0), 3);
}

#[tokio::test]
async fn a_line_the_ledger_cant_take_is_kept_and_written_with_the_next() {
    let dir = Dir::new("kept");
    // A file where the ledger's folder should be: nothing can be written
    // until it's gone.
    let folder = dir.path("ledger");
    std::fs::write(&folder, "in the way").unwrap();
    let mut driver = UsageDriver::start(Some(folder.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    driver.record(line(1, 5));
    driver.record(line(2, 5));
    // Through the same thread, so both lines have been tried by the time
    // it's answered.
    let (reply, answer) = oneshot::channel();
    driver.set_price(
        "claude-test-1".into(),
        Some(ModelPrice::new(1.0, 2.0)),
        reply,
    );
    match next(&mut driver).await {
        Report::PriceSet { reply, result } => {
            assert!(result.is_err(), "the folder is a file");
            let _ = reply.send(result.map_err(Error::Price));
        }
        _ => panic!("no prices changed"),
    }
    assert!(matches!(answer.await.unwrap(), Err(Error::Price(_))));
    std::fs::remove_file(&folder).unwrap();
    driver.record(line(3, 5));
    driver.finish().await;
    assert_eq!(turns_in(&folder), 3, "none lost");
}

#[tokio::test]
async fn lines_still_kept_as_it_finishes_are_tried_once_more() {
    let dir = Dir::new("last-try");
    let folder = dir.path("ledger");
    std::fs::write(&folder, "in the way").unwrap();
    let mut driver = UsageDriver::start(Some(folder.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    driver.record(line(1, 5));
    let (reply, answer) = oneshot::channel();
    driver.set_price("claude-test-1".into(), None, reply);
    let Report::PriceSet { reply, result } = next(&mut driver).await else {
        panic!("no prices changed")
    };
    let _ = reply.send(result.map_err(Error::Price));
    let _ = answer.await;
    std::fs::remove_file(&folder).unwrap();
    driver.finish().await;
    assert_eq!(turns_in(&folder), 1);
}

#[tokio::test]
async fn a_price_set_is_answered_after_the_prices_it_changed() {
    let dir = Dir::new("set");
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    let (reply, answer) = oneshot::channel();
    driver.set_price(
        "claude-test-1".into(),
        Some(ModelPrice::new(7.0, 8.0)),
        reply,
    );
    let table = prices(&mut driver).await;
    let own = table.find("claude-test-1").expect("the user's own");
    assert_eq!((own.price.input, own.layer), (7.0, PriceLayer::Own));
    let Report::PriceSet { reply, result } = next(&mut driver).await else {
        panic!("then the answer")
    };
    assert_eq!(result, Ok(()));
    let _ = reply.send(result.map_err(Error::Price));
    assert_eq!(answer.await.unwrap(), Ok(()));
    driver.finish().await;
}

#[tokio::test]
async fn prices_never_fetched_are_fetched_at_once_and_sent_again() {
    let dir = Dir::new("fetch");
    let (fetcher, count) = counting(Ok(litellm("claude-fetched-1")));
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        CHECK_EVERY,
    );
    // The fetch may be kept before the prices are first read: then both
    // have it.
    let mut fetched = prices(&mut driver).await;
    if fetched.fetched.is_none() {
        fetched = prices(&mut driver).await;
    }
    assert!(fetched.fetched.is_some(), "{fetched:?}");
    let model = fetched.find("claude-fetched-1").expect("fetched");
    assert_eq!(model.layer, PriceLayer::Fetched);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    driver.finish().await;
}

#[tokio::test]
async fn prices_fetched_less_than_a_day_ago_are_not_fetched_again() {
    let dir = Dir::new("fresh");
    prices::take_fetched(
        &dir.0,
        &litellm("claude-fetched-1"),
        Wall(wall_now().0 - 3_600_000),
    )
    .unwrap();
    let (fetcher, count) = counting(Ok(litellm("claude-fetched-2")));
    // Looking every few milliseconds, as another app might have fetched.
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        Duration::from_millis(5),
    );
    prices(&mut driver).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    driver.finish().await;
}

#[tokio::test]
async fn prices_a_day_old_are_fetched_once_and_then_are_fresh() {
    let dir = Dir::new("stale");
    let day_and_more = Wall(wall_now().0 - 25 * 3_600_000);
    prices::take_fetched(&dir.0, &litellm("claude-fetched-1"), day_and_more).unwrap();
    let (fetcher, count) = counting(Ok(litellm("claude-fetched-2")));
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        Duration::from_millis(5),
    );
    let mut fetched = prices(&mut driver).await;
    if fetched.find("claude-fetched-2").is_none() {
        fetched = prices(&mut driver).await;
    }
    assert!(fetched.find("claude-fetched-2").is_some(), "{fetched:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1, "fresh, once fetched");
    driver.finish().await;
}

#[tokio::test]
async fn a_failed_fetch_changes_nothing_and_is_tried_again() {
    let dir = Dir::new("offline");
    let (fetcher, count) = counting(Err("the machine seems to be offline".into()));
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        Duration::from_millis(5),
    );
    prices(&mut driver).await;
    let waited = tokio::time::timeout(PATIENCE, async {
        while count.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    waited.await.expect("tried again");
    let more = tokio::time::timeout(Duration::from_millis(100), driver.next()).await;
    assert!(more.is_err(), "no prices changed");
    assert_eq!(prices::fetched_at(&dir.0), None);
    driver.finish().await;
}

#[tokio::test]
async fn a_table_that_doesnt_read_is_not_kept() {
    let dir = Dir::new("broken");
    let (fetcher, count) = counting(Ok(b"<html>rate limited</html>".to_vec()));
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        Duration::from_millis(5),
    );
    prices(&mut driver).await;
    let waited = tokio::time::timeout(PATIENCE, async {
        while count.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    waited.await.expect("tried again");
    assert_eq!(prices::fetched_at(&dir.0), None);
    driver.finish().await;
}

#[tokio::test]
async fn finishing_doesnt_wait_for_a_fetch_under_way_and_it_keeps_nothing() {
    let dir = Dir::new("slow-fetch");
    let (started, fetching) = std::sync::mpsc::channel::<()>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    // Dropped with the fetcher, once the looking thread has ended.
    let (alive, ended) = std::sync::mpsc::channel::<()>();
    let fetcher: Fetcher = Box::new(move || {
        let _alive = &alive;
        let _ = started.send(());
        let _ = wait.recv();
        Ok(litellm("claude-fetched-1"))
    });
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        CHECK_EVERY,
    );
    prices(&mut driver).await;
    fetching.recv_timeout(PATIENCE).expect("fetching");
    (tokio::time::timeout(PATIENCE, driver.finish()).await).expect("finished");
    go.send(()).unwrap();
    assert!(
        matches!(
            ended.recv_timeout(PATIENCE),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ),
        "the looking thread ends"
    );
    assert_eq!(prices::fetched_at(&dir.0), None, "nothing kept");
}

/// A driver whose ledger thread is stuck until the sender it gives is
/// dropped, going on without it after `within`.
fn stuck(dir: &Dir, within: Duration) -> (UsageDriver, std::sync::mpsc::Sender<()>) {
    let (unstick, stuck) = std::sync::mpsc::channel();
    let options = Options {
        prices_within: within,
        finish_within: within,
        stuck: Some(stuck),
        ..Options::default()
    };
    let driver = UsageDriver::launch(Some(dir.0.clone()), "caf-test".into(), None, options);
    (driver, unstick)
}

#[tokio::test]
async fn with_the_thread_stuck_the_bundled_prices_are_given_and_the_rest_when_it_isnt() {
    let dir = Dir::new("stuck-prices");
    prices::set_own(&dir.0, "claude-test-1", Some(ModelPrice::new(3.0, 4.0))).unwrap();
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    assert_eq!(
        prices(&mut driver).await,
        prices::bundled(),
        "after a while"
    );
    // A reading for a summary isn't waited for for ever either.
    driver.reload(oneshot::channel().0);
    assert!(matches!(next(&mut driver).await, Report::Reloaded(_)));
    drop(unstick);
    let read = prices(&mut driver).await;
    assert!(read.find("claude-test-1").is_some(), "the prices read");
    // The reading given up on is let be, once it comes.
    let more = tokio::time::timeout(Duration::from_millis(200), driver.next()).await;
    assert!(more.is_err(), "{:?}", more.map(|_| ()));
    driver.finish().await;
}

#[tokio::test]
async fn finishing_waits_for_a_stuck_thread_only_so_long() {
    let dir = Dir::new("stuck-finish");
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    driver.record(line(1, 5));
    let finished = tokio::time::timeout(PATIENCE, driver.finish()).await;
    assert!(finished.is_ok(), "it isn't waited for for ever");
    drop(unstick);
}

#[tokio::test]
async fn a_line_kept_is_written_at_the_next_look_with_no_line_after_it() {
    let dir = Dir::new("hourly");
    let folder = dir.path("ledger");
    std::fs::write(&folder, "in the way").unwrap();
    let mut driver = UsageDriver::start(
        Some(folder.clone()),
        "caf-test".into(),
        None,
        Duration::from_millis(5),
    );
    prices(&mut driver).await;
    driver.record(line(1, 5));
    // Through the same thread, so the line has been tried by the time
    // it's answered.
    let (reply, answer) = oneshot::channel();
    driver.set_price("claude-test-1".into(), None, reply);
    let Report::PriceSet { reply, result } = next(&mut driver).await else {
        panic!("the price set")
    };
    let _ = reply.send(result.map_err(Error::Price));
    let _ = answer.await;
    std::fs::remove_file(&folder).unwrap();
    let written = tokio::time::timeout(PATIENCE, async {
        loop {
            // The driver hands each look to the thread as it's polled.
            let _ = tokio::time::timeout(Duration::from_millis(5), driver.next()).await;
            if folder.is_dir() && turns_in(&folder) == 1 {
                return;
            }
        }
    });
    written.await.expect("written at a look");
    driver.finish().await;
}

#[test]
fn a_table_is_due_if_never_fetched_a_day_old_or_from_the_future() {
    let now = Wall(1_800_000_000_000);
    let hour = 3_600_000;
    assert!(due(None, now));
    assert!(!due(Some(Wall(now.0 - hour)), now));
    assert!(!due(Some(Wall(now.0 - 23 * hour)), now));
    assert!(due(Some(Wall(now.0 - 24 * hour)), now));
    assert!(due(Some(Wall(now.0 + hour)), now), "the clock went back");
}

/// Serves one HTTP answer of `body` on this machine, and returns its URL.
fn serve_once(body: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/prices.json", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
    });
    url
}

#[test]
fn a_fetch_gets_the_whole_table() {
    let json = litellm("claude-fetched-1");
    assert_eq!(fetch(&serve_once(json.clone()), false), Ok(json));
}

#[test]
fn a_fetch_far_bigger_than_the_table_fails() {
    let big = vec![b' '; usize::try_from(MOST).unwrap() + 1];
    let fetched = fetch(&serve_once(big), false);
    assert_eq!(fetched, Err("it was far bigger than it should be".into()));
}

#[test]
fn a_fetch_is_over_https_only() {
    let fetched = fetch(&serve_once(litellm("claude-fetched-1")), true);
    assert_eq!(
        fetched,
        Err("it was sent somewhere that isn't https".into())
    );
}

#[test]
fn a_fetch_that_finds_nothing_says_so() {
    // A port nothing listens on.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/prices.json", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        fetch(&url, false),
        Err("the machine seems to be offline".into())
    );
}

#[tokio::test]
async fn a_price_another_app_set_is_taken_at_the_next_look() {
    let dir = Dir::new("another-app");
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        None,
        Duration::from_millis(5),
    );
    prices(&mut driver).await;
    // Looking every few milliseconds, prices that haven't changed aren't
    // sent again.
    let more = tokio::time::timeout(Duration::from_millis(100), driver.next()).await;
    assert!(more.is_err(), "nothing changed");
    prices::set_own(&dir.0, "claude-test-1", Some(ModelPrice::new(9.0, 9.0))).unwrap();
    let table = prices(&mut driver).await;
    let own = table.find("claude-test-1").expect("the other app's");
    assert_eq!(own.layer, PriceLayer::Own);
    driver.finish().await;
}

#[tokio::test]
async fn prices_read_again_for_a_summary_are_sent_first_if_they_changed() {
    let dir = Dir::new("reload");
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    driver.reload(oneshot::channel().0);
    assert!(
        matches!(next(&mut driver).await, Report::Reloaded(_)),
        "unchanged, so no prices first"
    );
    prices::set_own(&dir.0, "claude-test-1", Some(ModelPrice::new(9.0, 9.0))).unwrap();
    driver.reload(oneshot::channel().0);
    let table = prices(&mut driver).await;
    assert!(table.find("claude-test-1").is_some());
    assert!(matches!(next(&mut driver).await, Report::Reloaded(_)));
    driver.finish().await;
}

/// Holds the ledger's lock for app `caf-test` in `folder`, as another
/// program would, until the file is dropped.
fn hold_lock(folder: &Path) -> std::fs::File {
    let app = folder.join("caf-test");
    std::fs::create_dir_all(&app).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(app.join(".lock"))
        .unwrap();
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).unwrap();
    file
}

/// A driver for the ledger in `dir`, as CAF runs it: a minute without
/// trying after a lock given up on, and an hour between looks.
fn cooling(dir: &Dir) -> UsageDriver {
    UsageDriver::launch(
        Some(dir.0.clone()),
        "caf-test".into(),
        None,
        Options::default(),
    )
}

/// Sets no price, and says how long the ledger's thread took to answer:
/// so how long the work before it took.
async fn answered_in(driver: &mut UsageDriver) -> Duration {
    let started = std::time::Instant::now();
    let (reply, answer) = oneshot::channel();
    driver.set_price("claude-test-1".into(), None, reply);
    loop {
        if let Report::PriceSet { reply, result } = next(driver).await {
            let _ = reply.send(result.map_err(Error::Price));
            break;
        }
    }
    let _ = answer.await;
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_a_lock_is_given_up_on_lines_are_kept_untried_and_shutting_down_tries_once() {
    let dir = Dir::new("lock-held");
    let lock = hold_lock(&dir.0);
    let mut driver = cooling(&dir);
    prices(&mut driver).await;
    driver.record(line(1, 5));
    let first = answered_in(&mut driver).await;
    assert!(
        first >= Duration::from_secs(5),
        "the lock is tried for five seconds: {first:?}"
    );
    for turn in 2..=4 {
        driver.record(line(turn, 5));
    }
    let then = answered_in(&mut driver).await;
    assert!(then < Duration::from_secs(4), "kept, not tried: {then:?}");
    let started = std::time::Instant::now();
    driver.finish().await;
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(5) && took < Duration::from_secs(9),
        "one try as it finishes: {took:?}"
    );
    drop(lock);
    assert_eq!(turns_in(&dir.0), 0, "lost, as reported");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lines_kept_behind_a_lock_are_written_once_it_is_let_go() {
    let dir = Dir::new("lock-let-go");
    let lock = hold_lock(&dir.0);
    let mut driver = cooling(&dir);
    prices(&mut driver).await;
    driver.record(line(1, 5));
    driver.record(line(2, 5));
    answered_in(&mut driver).await;
    drop(lock);
    driver.finish().await;
    assert_eq!(turns_in(&dir.0), 2);
}

#[tokio::test]
async fn a_price_set_the_stuck_thread_doesnt_answer_is_answered_that_it_didnt() {
    let dir = Dir::new("stuck-set");
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    assert_eq!(prices(&mut driver).await, prices::bundled());
    let (reply, _answer) = oneshot::channel();
    driver.set_price(
        "claude-test-1".into(),
        Some(ModelPrice::new(1.0, 2.0)),
        reply,
    );
    let Report::PriceSet { result, .. } = next(&mut driver).await else {
        panic!("the price set")
    };
    assert_eq!(result, Err(NO_ANSWER.into()));
    // Once the thread goes on, its own answer is let be.
    drop(unstick);
    let more = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if let Report::PriceSet { .. } = driver.next().await {
                return;
            }
        }
    });
    assert!(more.await.is_err(), "answered once");
    driver.finish().await;
}

#[tokio::test]
async fn a_price_set_still_waiting_as_the_driver_finishes_hears_it_has_gone() {
    let dir = Dir::new("stuck-set-finish");
    let (unstick, stuck) = std::sync::mpsc::channel();
    let options = Options {
        finish_within: Duration::from_millis(50),
        stuck: Some(stuck),
        ..Options::default()
    };
    let mut driver = UsageDriver::launch(Some(dir.0.clone()), "caf-test".into(), None, options);
    let (reply, answer) = oneshot::channel();
    driver.set_price("claude-test-1".into(), None, reply);
    driver.finish().await;
    assert!(answer.await.is_err(), "let go");
    drop(unstick);
}

#[tokio::test]
async fn a_reading_asked_for_while_one_is_under_way_waits_for_another_after_it() {
    let dir = Dir::new("reading-after");
    let (unstick, stuck) = std::sync::mpsc::channel();
    let options = Options {
        prices_within: PATIENCE,
        stuck: Some(stuck),
        ..Options::default()
    };
    let mut driver = UsageDriver::launch(Some(dir.0.clone()), "caf-test".into(), None, options);
    let (first, _first) = oneshot::channel();
    driver.reload(first);
    let (second, _second) = oneshot::channel();
    driver.reload(second);
    let (third, _third) = oneshot::channel();
    driver.reload(third);
    drop(unstick);
    let mut batches = Vec::new();
    while batches.len() < 2 {
        if let Report::Reloaded(waiting) = next(&mut driver).await {
            batches.push(waiting.len());
        }
    }
    assert_eq!(
        batches,
        [1, 2],
        "the first reading, then one more for the rest"
    );
    let more = tokio::time::timeout(Duration::from_millis(200), driver.next()).await;
    assert!(more.is_err(), "and no more");
    driver.finish().await;
}

/// Claude's limits: its five hours `used`, heard `ago` ms before now,
/// resetting in four hours.
fn limits(used: u8, ago: i64) -> Box<ProfileLimits> {
    let now = wall_now().0;
    let limits = serde_json::json!({
        "windows": [{
            "kind": {"type": "FiveHours"},
            "used": used,
            "resets": now + 4 * 3_600_000,
            "state": {"type": "Fine"},
            "heard": now - ago,
        }],
        "extra_usage": null,
        "cleared": null,
    });
    let limits = serde_json::from_value(limits).unwrap();
    Box::new(ProfileLimits::new(
        caf_core::profiles::Builtin::ClaudeAgentAcp,
        limits,
    ))
}

/// The limits kept in `folder`, now.
fn kept(folder: &Path) -> Vec<ProfileLimits> {
    let (kept, errors) = caf_usage::limits::load(folder, wall_now());
    assert!(errors.is_empty(), "{errors:?}");
    kept
}

/// That the driver says nothing more for a while.
async fn quiet(driver: &mut UsageDriver) {
    let more = tokio::time::timeout(Duration::from_millis(200), driver.next()).await;
    assert!(more.is_err(), "nothing more: {:?}", more.map(|_| ()));
}

#[tokio::test]
async fn with_no_ledger_limits_are_dropped() {
    let mut driver = UsageDriver::start(None, "caf-test".into(), None, CHECK_EVERY);
    assert_eq!(prices(&mut driver).await, prices::bundled());
    driver.save_limits(limits(40, 1000));
    quiet(&mut driver).await;
    driver.finish().await;
}

#[tokio::test]
async fn with_no_limits_kept_none_are_reported() {
    let dir = Dir::new("limits-none");
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    quiet(&mut driver).await;
    driver.finish().await;
}

#[tokio::test]
async fn the_limits_kept_are_reported_after_the_first_prices() {
    let dir = Dir::new("limits-start");
    caf_usage::limits::save(&dir.0, &limits(40, 1000), wall_now()).unwrap();
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    match next(&mut driver).await {
        Report::Limits(given) => assert_eq!(given, kept(&dir.0)),
        _ => panic!("the limits kept"),
    }
    driver.finish().await;
}

#[tokio::test]
async fn a_save_is_kept_and_news_another_app_kept_is_reported() {
    let dir = Dir::new("limits-save");
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    driver.save_limits(limits(40, 2000));
    // Saved: nothing was news.
    let started = std::time::Instant::now();
    while kept(&dir.0).is_empty() {
        assert!(started.elapsed() < PATIENCE, "it's kept");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    quiet(&mut driver).await;
    // Another app hears later news, and a save of older news takes it in.
    caf_usage::limits::save(&dir.0, &limits(60, 1000), wall_now()).unwrap();
    driver.save_limits(limits(50, 1500));
    let later = kept(&dir.0);
    match next(&mut driver).await {
        Report::Limits(given) => assert_eq!(given, later, "the later news"),
        _ => panic!("the limits kept"),
    }
    assert_eq!(kept(&dir.0), later, "never undone");
    driver.finish().await;
}

#[tokio::test]
async fn with_the_thread_stuck_a_save_waits_and_a_reading_doesnt() {
    let dir = Dir::new("stuck-limits");
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    let given = limits(40, 1000);
    driver.save_limits(given.clone());
    assert_eq!(
        prices(&mut driver).await,
        prices::bundled(),
        "after a while"
    );
    let started = std::time::Instant::now();
    driver.reload(oneshot::channel().0);
    assert!(matches!(next(&mut driver).await, Report::Reloaded(_)));
    assert!(started.elapsed() < PATIENCE, "answered without the thread");
    drop(unstick);
    prices(&mut driver).await;
    driver.finish().await;
    assert_eq!(kept(&dir.0), [*given], "saved once it isn't stuck");
}

#[tokio::test]
async fn finishing_with_a_save_the_stuck_thread_hasnt_made_waits_only_so_long() {
    let dir = Dir::new("stuck-limits-finish");
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    driver.save_limits(limits(40, 1000));
    let finished = tokio::time::timeout(PATIENCE, driver.finish()).await;
    assert!(finished.is_ok(), "it isn't waited for for ever");
    drop(unstick);
}

#[tokio::test]
async fn saves_queued_together_are_one_with_the_latest_news_of_each() {
    let dir = Dir::new("limits-queued");
    let (mut driver, unstick) = stuck(&dir, Duration::from_millis(50));
    let latest = limits(40, 1000);
    driver.save_limits(latest.clone());
    // Older news, as the core never sends: kept apart, it'd be told.
    driver.save_limits(limits(30, 2000));
    assert_eq!(prices(&mut driver).await, prices::bundled());
    drop(unstick);
    prices(&mut driver).await;
    quiet(&mut driver).await;
    driver.finish().await;
    assert_eq!(kept(&dir.0), [*latest]);
}

/// Holds the limits' lock in `dir`, as another program would, until the
/// file is dropped.
fn hold_limits_lock(dir: &Dir) -> std::fs::File {
    let folder = dir.0.join("limits");
    std::fs::create_dir_all(&folder).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(folder.join(".lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    lock
}

/// A driver for the ledger in `dir`, an hour between looks, that tries
/// nothing behind a lock given up on for `cooldown`.
fn cooling_for(dir: &Dir, cooldown: Duration) -> UsageDriver {
    let options = Options {
        cooldown,
        ..Options::default()
    };
    UsageDriver::launch(Some(dir.0.clone()), "caf-test".into(), None, options)
}

/// Sets no price, and says how long since `started` the ledger's thread
/// answered: so how long the work before it took.
async fn answered_since(driver: &mut UsageDriver, started: std::time::Instant) -> Duration {
    answered_in(driver).await;
    started.elapsed()
}

/// A profile's limits cleared `ago` ms before now, with no window.
fn cleared(ago: i64) -> Box<ProfileLimits> {
    let mut cleared = limits(0, 0);
    cleared.limits.windows.clear();
    cleared.limits.cleared = Some(Wall(wall_now().0 - ago));
    cleared
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_a_lock_on_the_limits_is_given_up_on_saves_arent_tried_even_as_it_finishes() {
    let dir = Dir::new("limits-lock-held");
    let lock = hold_limits_lock(&dir);
    let mut driver = cooling(&dir);
    prices(&mut driver).await;
    let started = std::time::Instant::now();
    driver.save_limits(limits(40, 1000));
    let first = answered_since(&mut driver, started).await;
    assert!(
        first >= Duration::from_secs(5),
        "the lock is tried for five seconds: {first:?}"
    );
    for used in [50, 60, 70] {
        driver.save_limits(limits(used, 1000));
    }
    let started = std::time::Instant::now();
    driver.finish().await;
    let took = started.elapsed();
    assert!(took < Duration::from_secs(4), "not tried: {took:?}");
    drop(lock);
    assert!(kept(&dir.0).is_empty(), "never kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_the_cooldown_has_passed_the_latest_unsaved_are_saved_with_the_next() {
    let dir = Dir::new("limits-cooled");
    let cooldown = Duration::from_secs(3);
    let lock = hold_limits_lock(&dir);
    let mut driver = cooling_for(&dir, cooldown);
    prices(&mut driver).await;
    driver.save_limits(limits(40, 3000));
    answered_in(&mut driver).await;
    let clearing = cleared(2000);
    driver.save_limits(clearing.clone());
    let then = answered_in(&mut driver).await;
    assert!(then < Duration::from_secs(2), "not tried: {then:?}");
    drop(lock);
    tokio::time::sleep(cooldown + Duration::from_millis(500)).await;
    assert!(kept(&dir.0).is_empty(), "nothing tried meanwhile");
    let news = limits(70, 1000);
    driver.save_limits(news.clone());
    answered_in(&mut driver).await;
    let mut expected = clearing.limits.clone();
    expected.merge(&news.limits);
    let kept: Vec<_> = (kept(&dir.0).into_iter()).map(|kept| kept.limits).collect();
    assert_eq!(kept, [expected], "both");
    driver.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clearing_saved_while_cooling_is_kept_as_it_finishes() {
    let dir = Dir::new("limits-cooled-finish");
    let cooldown = Duration::from_secs(3);
    let lock = hold_limits_lock(&dir);
    let mut driver = cooling_for(&dir, cooldown);
    prices(&mut driver).await;
    driver.save_limits(limits(40, 3000));
    answered_in(&mut driver).await;
    let clearing = cleared(2000);
    driver.save_limits(clearing.clone());
    answered_in(&mut driver).await;
    drop(lock);
    tokio::time::sleep(cooldown + Duration::from_millis(500)).await;
    driver.finish().await;
    assert_eq!(kept(&dir.0), [*clearing]);
}

#[tokio::test]
async fn a_cleared_only_file_is_reported() {
    let dir = Dir::new("limits-cleared-only");
    let clearing = cleared(1000);
    caf_usage::limits::save(&dir.0, &clearing, wall_now()).unwrap();
    let mut driver = UsageDriver::start(Some(dir.0.clone()), "caf-test".into(), None, CHECK_EVERY);
    prices(&mut driver).await;
    match next(&mut driver).await {
        Report::Limits(given) => assert_eq!(given, [*clearing]),
        _ => panic!("the limits kept"),
    }
    driver.finish().await;
}

/// Every record logged while the tests run, by level: each test finds its
/// own by its folder, which they name.
static LOGGED: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

/// Keeps what CAF logs, in [`LOGGED`].
struct Logged;

impl log::Log for Logged {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "caf" && metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let text = record.args().to_string();
        let mut logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
        logged.push((record.level(), text));
    }

    fn flush(&self) {}
}

/// What's been logged that names `folder`, once the logger's in place.
fn logged(folder: &Path) -> Vec<(log::Level, String)> {
    let folder = folder.display().to_string();
    let logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
    (logged.iter())
        .filter(|(_, text)| text.contains(&folder))
        .cloned()
        .collect()
}

/// Puts [`Logged`] in place, if it isn't already.
fn logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&Logged).expect("the tests' logger");
        log::set_max_level(log::LevelFilter::Debug);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_given_up_on_is_reported_as_cooling() {
    logging();
    let dir = Dir::new("limits-lock-said");
    let lock = hold_limits_lock(&dir);
    let mut driver = cooling(&dir);
    prices(&mut driver).await;
    driver.save_limits(limits(40, 1000));
    answered_in(&mut driver).await;
    drop(lock);
    let said = logged(&dir.0);
    let [(level, text)] = said.as_slice() else {
        panic!("one report: {said:?}");
    };
    assert_eq!(*level, log::Level::Warn);
    assert!(
        text.contains("limits/.lock") && text.contains("not tried again for 60 s"),
        "{text}"
    );
    driver.finish().await;
}

#[tokio::test]
async fn a_save_that_panics_is_reported_once_in_a_row_and_dropped() {
    logging();
    let dir = Dir::new("limits-panics");
    let options = Options {
        save_limits: |_, _, _| panic!("a save that panics"),
        ..Options::default()
    };
    let mut driver = UsageDriver::launch(Some(dir.0.clone()), "caf-test".into(), None, options);
    prices(&mut driver).await;
    for used in [40, 50] {
        driver.save_limits(limits(used, 1000));
        answered_in(&mut driver).await;
    }
    driver.finish().await;
    let levels: Vec<log::Level> = logged(&dir.0).into_iter().map(|(level, _)| level).collect();
    assert_eq!(
        levels,
        [log::Level::Warn, log::Level::Debug],
        "a warning, then not again; nothing lost as it finishes"
    );
}

#[test]
fn a_failing_save_followed_by_the_hourly_look_is_tried_once_before_it() {
    static TRIES: AtomicUsize = AtomicUsize::new(0);
    let dir = Dir::new("limits-tried-once");
    let ledger = Ledger::new(dir.0.clone(), "caf-test");
    let (work, inbox) = std::sync::mpsc::channel();
    let (tell, _told) = mpsc::unbounded_channel();
    let save: Saver = |_, _, _| {
        TRIES.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("it fails"))
    };
    work.send(Work::SaveLimits(limits(40, 1000))).unwrap();
    work.send(Work::Reload(None)).unwrap();
    drop(work);
    let limits = Limits::new(LOCK_COOLDOWN, save);
    keep(
        Some(&ledger),
        &inbox,
        &tell,
        &AtomicUsize::new(0),
        LOCK_COOLDOWN,
        limits,
    );
    assert_eq!(
        TRIES.load(Ordering::SeqCst),
        2,
        "before the look, not again in it, and once as it ends"
    );
}
