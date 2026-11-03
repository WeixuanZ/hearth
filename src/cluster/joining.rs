//! Joining a group: asking, the code a person types on the member, the
//! group key sealed with the secret both sides agreed, and the commands
//! that answer a request to join.

use super::*;

pub(super) struct Asking {
    pub(super) name: String,
    pub(super) code: String,
    pub(super) fails: u8,
    pub(super) answer: Option<oneshot::Sender<bool>>,
}
pub(super) struct Joining {
    pub(super) host: String,
    pub(super) code: Option<String>,
    /// Set when a person says they typed the code on the member. Dropping it
    /// stops the join.
    pub(super) confirmed: watch::Sender<bool>,
}
/// A computer asks to join. It gets the key once someone types its code here.
pub(super) async fn admit(mut stream: TcpStream, name: String, public: &str) -> Result<()> {
    let refuse = |reason: &str| Message::Refused {
        reason: reason.into(),
    };
    let (me, me_name, key, busy) = with(|group| {
        (
            group.id.clone(),
            group.name.clone(),
            group.key.clone(),
            group.asking.is_some(),
        )
    })?;
    let Some(key) = key.filter(|_| !busy) else {
        return wire::send(
            &mut stream,
            &refuse("this computer cannot take a request now"),
        )
        .await;
    };
    let theirs = crypto::decode_key(public)?;
    let ours = Ephemeral::new();
    let our_public = ours.public;
    let message = Message::Joining {
        id: me,
        name: me_name,
        public: crypto::encode(&our_public),
    };
    wire::send(&mut stream, &message).await?;
    let shared = ours.agree(&theirs);
    let (answer, answered) = oneshot::channel();
    with(|group| {
        group.asking = Some(Asking {
            name: name.clone(),
            code: crypto::code(&shared, &theirs, &our_public),
            fails: 0,
            answer: Some(answer),
        })
    })?;
    log::info!("{name} asks to join");
    let mut byte = [0u8; 1];
    let approved = tokio::select! {
        answer = timeout(ASKING, answered) => matches!(answer, Ok(Ok(true))),
        _ = stream.read(&mut byte) => false,
    };
    with(|group| group.asking = None)?;
    if !approved {
        return wire::send(&mut stream, &refuse("not approved")).await;
    }
    let secret = crypto::derive(&shared, b"hearth-join");
    let sealed = crypto::seal(
        &secret,
        &[theirs, our_public].concat(),
        &serde_json::to_vec(&key)?,
    );
    let message = Message::Granted {
        sealed: crypto::encode(&sealed),
    };
    wire::send(&mut stream, &message).await
}
pub(super) fn ask_to_join(member: &Heard) {
    let addr = SocketAddr::new(member.addr, member.hello.port);
    let (confirmed, confirm) = watch::channel(false);
    let Ok((id, name)) = with(|group| {
        group.joining = Some(Joining {
            host: member.hello.name.clone(),
            code: None,
            confirmed,
        });
        (group.id.clone(), group.name.clone())
    }) else {
        return;
    };
    let host = member.hello.name.clone();
    tokio::spawn(async move {
        let result = join(addr, id, name, confirm).await;
        let _ = with(|group| {
            group.joining = None;
            match result {
                Ok(key) => match key.save() {
                    Ok(()) => {
                        log::info!("joined {host}'s group");
                        group.key = Some(key);
                        group.target = None;
                    }
                    Err(error) => log::warn!("could not keep the group key: {error:#}"),
                },
                Err(error) => {
                    log::info!("did not join {host}: {error:#}");
                    group.separate = true;
                }
            }
        });
    });
}
pub(super) async fn join(
    addr: SocketAddr,
    id: String,
    name: String,
    mut confirm: watch::Receiver<bool>,
) -> Result<GroupKey> {
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(addr)).await??;
    let Message::Nonce { .. } = timeout(ANSWER, wire::receive(&mut stream)).await?? else {
        anyhow::bail!("the other computer did not start properly");
    };
    let ours = Ephemeral::new();
    let our_public = ours.public;
    let message = Message::Join {
        id,
        name,
        public: crypto::encode(&our_public),
    };
    wire::send(&mut stream, &message).await?;
    let Message::Joining { public, .. } = timeout(ANSWER, wire::receive(&mut stream)).await??
    else {
        anyhow::bail!("the other computer cannot take a request now");
    };
    let theirs = crypto::decode_key(&public)?;
    let shared = ours.agree(&theirs);
    let code = crypto::code(&shared, &our_public, &theirs);
    with(|group| {
        if let Some(joining) = group.joining.as_mut() {
            joining.code = Some(code);
        }
    })?;

    let message = tokio::select! {
        message = timeout(ASKING, wire::receive(&mut stream)) => message??,
        _ = stopped(&mut confirm) => anyhow::bail!("stopped asking"),
    };
    let sealed = match message {
        Message::Granted { sealed } => sealed,
        Message::Refused { reason } => anyhow::bail!("{reason}"),
        _ => anyhow::bail!("an unexpected answer"),
    };
    // Only a person can tell this came from the member they typed the code
    // on, and not from someone pretending to be one.
    while !*confirm.borrow_and_update() {
        confirm.changed().await.context("stopped asking")?;
    }
    let secret = crypto::derive(&shared, b"hearth-join");
    let key = crypto::open(
        &secret,
        &[our_public, theirs].concat(),
        &crypto::decode(&sealed)?,
    )?;
    Ok(serde_json::from_slice(&key)?)
}
/// Waits until the join is called off.
pub(super) async fn stopped(confirm: &mut watch::Receiver<bool>) {
    while confirm.changed().await.is_ok() {}
}
pub(super) fn spaced(code: &str) -> String {
    format!("{} {}", &code[..3], &code[3..])
}
/// Approves the computer asking to join, if `typed` is the code it shows.
pub fn approve(typed: &str) -> Result<()> {
    with(|group| {
        let asking = group
            .asking
            .as_mut()
            .context("no computer is asking to join")?;
        if crypto::codes_match(&asking.code, typed) {
            let _ = asking.answer.take().map(|answer| answer.send(true));
            return Ok(());
        }
        asking.fails += 1;
        if asking.fails >= 5 {
            let _ = asking.answer.take().map(|answer| answer.send(false));
        }
        Err(anyhow!("that is not the code {} shows", asking.name))
    })?
}
pub fn decline() -> Result<()> {
    with(|group| {
        let asking = group
            .asking
            .as_mut()
            .context("no computer is asking to join")?;
        let _ = asking.answer.take().map(|answer| answer.send(false));
        Ok(())
    })?
}
/// Says the code was typed on the member, which finishes joining.
pub fn confirm() -> Result<()> {
    with(|group| {
        let joining = group
            .joining
            .as_ref()
            .context("this computer is not asking to join")?;
        joining.confirmed.send_replace(true);
        Ok(())
    })?
}
/// Stops asking to join, and keeps a group of this computer's own.
pub fn separate() -> Result<()> {
    with(|group| {
        group.joining = None;
        group.separate = true;
        group.target = None;
    })
}
/// Leaves this group to ask to join the other one on the network.
pub fn join_other() -> Result<()> {
    let target = with(|group| group.other.as_ref().map(|(id, _)| id.clone()))?
        .context("there is no other group on the network")?;
    GroupKey::delete()?;
    with(|group| {
        group.key = None;
        group.target = Some(target);
        group.separate = false;
    })
}
