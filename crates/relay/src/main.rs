//! `caf-relay`: see the library's docs. Run with nothing it's asked for,
//! it says what it is for and fails.

fn main() {
    caf_relay::relay_if_asked();
    eprintln!(
        "caf-relay: CAF starts this program for an agent, with {} set; it does nothing on its own",
        caf_relay::RELAY_VAR
    );
    std::process::exit(2);
}
