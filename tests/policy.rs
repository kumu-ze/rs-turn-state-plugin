use rs_turn_state_plugin::model::{TicketAccountPolicy, TicketMode};
use rs_turn_state_plugin::{Store, Ticket, import, read_frame};

fn store() -> Store {
    let mut store = Store::default();
    store.settings.enabled = true;
    store.settings.inject = true;
    store.settings.require_ticket = true;
    store.settings.accounts.insert(
        "acct_one".into(),
        TicketAccountPolicy {
            mode: TicketMode::Manual,
            target_length: None,
        },
    );
    store.tickets.push(Ticket {
        account_id: "acct_one".into(),
        credential_revision:1,
        auth_binding: Some("a".repeat(64)),
        model: "gpt-6-astra".into(),
        state: format!("gAAAAA{}", "x".repeat(286)),
        expires_at: 2000,
    });
    store
}
#[test]
fn import_preserves_source_and_private_fields_but_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("old.json");
    let dest = root.path().join("new.json");
    let mut store = store();
    store.retained.insert(
        "proxy_pool".into(),
        serde_json::json!(["http://test:fixture@127.0.0.1:1"]),
    );
    let mut unbound = store.tickets[0].clone();
    unbound.auth_binding = None;
    store.tickets.push(unbound);
    let bytes = serde_json::to_vec(&store).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    assert_eq!(import(&source, &dest, 1000).unwrap(), 1);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    let loaded = Store::load(&dest, 1000).unwrap();
    assert_eq!(loaded.retained, store.retained);
    assert!(import(&source, &dest, 1000).is_err());
}
#[test]
fn rejects_partial_and_oversized_frames() {
    assert!(read_frame(&mut &b"\0\0"[..]).is_err());
    assert!(read_frame(&mut &1048577u32.to_be_bytes()[..]).is_err());
    assert!(read_frame(&mut &b""[..]).unwrap().is_none());
}
