//! Who serves: the lowest id among those that hear each other, and one
//! that serves keeps serving, so a laptop that wakes does not take Home back.

use super::*;

/// Without a group: ask to join one, or start one.
pub(super) fn look(
    id: &str,
    started: u64,
    now: u64,
    heard: &BTreeMap<String, Heard>,
) -> Result<()> {
    let (joining, separate, target) = with(|group| {
        group.role = Role::Looking;
        group.leader = None;
        group.members.clear();
        (
            group.joining.is_some(),
            group.separate,
            group.target.clone(),
        )
    })?;
    if joining {
        return Ok(());
    }
    let member = heard
        .values()
        .filter(|peer| !peer.hello.group.is_empty())
        .filter(|peer| {
            target
                .as_ref()
                .is_none_or(|target| *target == peer.hello.group)
        })
        .min_by_key(|peer| (!peer.hello.serving, peer.hello.id.clone()));
    match member {
        Some(member) if !separate => {
            ask_to_join(member);
            Ok(())
        }
        _ if separate || should_found(id, started, now, heard) => found(),
        _ => Ok(()),
    }
}
pub(super) fn found() -> Result<()> {
    let key = GroupKey::new();
    key.save()?;
    log::info!("started a group");
    with(|group| {
        group.key = Some(key);
        group.target = None;
        group.separate = false;
    })
}
/// Who serves Home. A member that is serving keeps it, the lowest id if two
/// are. If none is, the lowest id serves, once a new process has had time to
/// hear the others.
pub(super) fn role(me: &str, serving: bool, members: &[&Hello], now: u64, started: u64) -> Role {
    let standby = |hello: &Hello| Role::Standby {
        leader: hello.name.clone(),
    };
    let lowest_serving = members
        .iter()
        .filter(|hello| hello.serving)
        .min_by_key(|hello| hello.id.as_str());
    match lowest_serving {
        Some(hello) if !serving || hello.id.as_str() < me => standby(hello),
        Some(_) => Role::Serving,
        None if serving => Role::Serving,
        None if now.saturating_sub(started) < GRACE_MS => Role::Looking,
        None => match members.iter().min_by_key(|hello| hello.id.as_str()) {
            Some(hello) if hello.id.as_str() < me => standby(hello),
            _ => Role::Serving,
        },
    }
}
/// Whether a computer without a group should start one: nobody else has
/// one, and nobody with a lower id is about to.
pub(super) fn should_found(
    me: &str,
    started: u64,
    now: u64,
    heard: &BTreeMap<String, Heard>,
) -> bool {
    let waited = now.saturating_sub(started);
    let lower = heard.values().any(|peer| peer.hello.id.as_str() < me);
    let grouped = heard.values().any(|peer| !peer.hello.group.is_empty());
    !grouped && waited >= FOUND_MS && (!lower || waited >= LOWER_MS)
}
