//! Where an app keeps its chats by default. Alone in its own test
//! program, as it changes the environment.

use std::env;
use std::path::PathBuf;

use caf_save::FileStore;

#[test]
fn chats_are_kept_where_xdg_says() {
    let root = || FileStore::default_root("framewright");
    // SAFETY: this is the only test in its program, so nothing else reads
    // the environment meanwhile.
    unsafe {
        env::set_var("HOME", "/home/me");
        env::set_var("XDG_DATA_HOME", "/data");
    }
    assert_eq!(root(), Some(PathBuf::from("/data/framewright/chats")));
    // SAFETY: as above.
    unsafe { env::set_var("XDG_DATA_HOME", "data") };
    let home = Some(PathBuf::from("/home/me/.local/share/framewright/chats"));
    assert_eq!(root(), home, "a relative one is ignored");
    // SAFETY: as above.
    unsafe { env::remove_var("XDG_DATA_HOME") };
    assert_eq!(root(), home);
    // SAFETY: as above.
    unsafe { env::remove_var("HOME") };
    assert_eq!(root(), None);
}
