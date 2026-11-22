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
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let fetcher: Fetcher = Box::new(move || {
        let _ = wait.recv_timeout(PATIENCE);
        Ok(litellm("claude-fetched-1"))
    });
    let mut driver = UsageDriver::start(
        Some(dir.0.clone()),
        "caf-test".into(),
        Some(fetcher),
        CHECK_EVERY,
    );
    prices(&mut driver).await;
    (tokio::time::timeout(PATIENCE, driver.finish()).await).expect("finished");
    go.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(prices::fetched_at(&dir.0), None);
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
    let (reply, _answer) = oneshot::channel();
    driver.reload(reply);
    assert!(
        matches!(next(&mut driver).await, Report::Reloaded(_)),
        "unchanged, so no prices first"
    );
    prices::set_own(&dir.0, "claude-test-1", Some(ModelPrice::new(9.0, 9.0))).unwrap();
    let (reply, _answer) = oneshot::channel();
    driver.reload(reply);
    let table = prices(&mut driver).await;
    assert!(table.find("claude-test-1").is_some());
    assert!(matches!(next(&mut driver).await, Report::Reloaded(_)));
    driver.finish().await;
}
