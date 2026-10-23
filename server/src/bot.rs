//! The bot's commands, which only the chat of the configuration may give:
//! invite someone, revoke them, list who is invited.
//!
//! The bot asks Telegram for the messages it receives (long polling), and
//! keeps the number of the last one handled in `telegram-offset`, so that a
//! restart handles each message once. Messages from anyone else are left
//! unanswered.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use x2d_protocol::Role;

use crate::people::People;
use crate::telegram::Telegram;

/// The commands offered, as the chat's menu shows them.
const COMMANDS: &[(&str, &str)] = &[
    ("invite", "Inviter : /invite Max, ou /invite Max operateur"),
    ("revoquer", "Retirer l'accès : /revoquer Max"),
    ("personnes", "Les personnes invitées"),
    ("aide", "Ce que je sais faire"),
];

/// How many days an invitation sent from the bot can be accepted for.
const INVITATION_DAYS: u32 = 7;

/// Answer the chat's commands, for as long as the server runs.
pub async fn obey(telegram: Arc<Telegram>, people: People, origin: String, data: PathBuf) {
    let offset_path = data.join("telegram-offset");
    let mut offset: i64 =
        std::fs::read_to_string(&offset_path).ok().and_then(|text| text.trim().parse().ok()).unwrap_or(0);
    let mut offered = false;
    loop {
        if !offered {
            match telegram.offer(COMMANDS).await {
                Ok(()) => offered = true,
                Err(error) => tracing::warn!("cannot offer the bot's commands: {error:#}"),
            }
        }
        let updates = match telegram.updates(offset).await {
            Ok(updates) => updates,
            Err(error) => {
                tracing::warn!("cannot hear the bot's messages: {error:#}");
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };
        for update in updates {
            let Some(id) = update.get("update_id").and_then(Value::as_i64) else { continue };
            offset = offset.max(id + 1);
            if let Some(text) = from_owner(&update, telegram.chat()) {
                let reply = answer(text, &people, &origin);
                if let Err(error) = telegram.text(&reply).await {
                    tracing::warn!("cannot answer the bot's owner: {error:#}");
                }
            }
            keep(&offset_path, offset);
        }
    }
}

fn keep(path: &Path, offset: i64) {
    if let Err(error) = std::fs::write(path, offset.to_string()) {
        tracing::warn!("cannot keep the bot's offset: {error}");
    }
}

/// The text of `update`, if it is a message of the owner in their chat.
fn from_owner(update: &Value, owner: i64) -> Option<&str> {
    let message = update.get("message")?;
    let chat = message.pointer("/chat/id").and_then(Value::as_i64);
    let sender = message.pointer("/from/id").and_then(Value::as_i64);
    if chat != Some(owner) || sender != Some(owner) {
        tracing::info!("the bot ignored a message from someone else");
        return None;
    }
    message.get("text").and_then(Value::as_str)
}

/// The reply to the owner's message `text`.
fn answer(text: &str, people: &People, origin: &str) -> String {
    let text = text.trim();
    let (command, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let command = command.split('@').next().unwrap_or(command).to_lowercase();
    let rest = rest.trim();
    let outcome = match command.as_str() {
        "/invite" | "/inviter" => invite(rest, people, origin),
        "/revoquer" | "/révoquer" | "/revoke" => revoke(rest, people),
        "/personnes" | "/people" => list(people),
        _ => return help(),
    };
    outcome.unwrap_or_else(|error| {
        tracing::warn!("a command of the bot failed: {error:#}");
        format!("Ça n'a pas marché : {error}")
    })
}

fn help() -> String {
    "Je peux :\n\
     /invite Max : un lien à usage unique pour que Max regarde l'imprimante ;\n\
     /invite Max operateur : pareil, avec les commandes ;\n\
     /revoquer Max : retirer l'accès de Max, sur tous ses navigateurs ;\n\
     /personnes : qui est invité.\n\
     Je préviens aussi ici de la fin des impressions, de leurs échecs et des alertes."
        .to_owned()
}

fn invite(rest: &str, people: &People, origin: &str) -> anyhow::Result<String> {
    let named = rest.rsplit_once(char::is_whitespace).and_then(|(name, last)| Some((name.trim(), role_named(last)?)));
    let (name, role) = named.unwrap_or((rest, Role::Spectator));
    if name.is_empty() || role_named(name).is_some() {
        return Ok("Dis-moi qui inviter : /invite Max, ou /invite Max operateur.".to_owned());
    }
    let token = people.invite(name, role, INVITATION_DAYS)?;
    Ok(format!(
        "Voici le lien de {name}, {}, valable {INVITATION_DAYS} jours et à usage unique :\n{origin}/invite/{token}",
        role.as_whom()
    ))
}

/// The role a word names, in French or in English, with or without accents.
fn role_named(word: &str) -> Option<Role> {
    match word.to_lowercase().as_str() {
        "operateur" | "opérateur" | "operator" => Some(Role::Operator),
        "spectateur" | "spectator" => Some(Role::Spectator),
        _ => None,
    }
}

fn revoke(name: &str, people: &People) -> anyhow::Result<String> {
    if name.is_empty() {
        return Ok("Dis-moi qui révoquer : /revoquer Max.".to_owned());
    }
    Ok(if people.revoke(name)? {
        format!("{name} n'a plus accès, sur aucun navigateur.")
    } else {
        format!("Je ne connais personne du nom de {name}. Les noms sont dans /personnes.")
    })
}

fn list(people: &People) -> anyhow::Result<String> {
    let listed = people.list()?;
    if listed.is_empty() {
        return Ok("Personne n'est invité.".to_owned());
    }
    let now = crate::now();
    let lines: Vec<String> = listed
        .iter()
        .map(|person| {
            let browsers = match person.logins {
                0 => "aucun navigateur".to_owned(),
                1 => "1 navigateur".to_owned(),
                count => format!("{count} navigateurs"),
            };
            let pending = person
                .invited_until
                .map_or_else(String::new, |until| format!(", lien valable encore {}", remaining(until - now)));
            format!("• {} ({}) : {browsers}{pending}", person.name, person.role.label())
        })
        .collect();
    Ok(lines.join("\n"))
}

/// `seconds` as the largest whole unit it holds.
fn remaining(seconds: i64) -> String {
    match seconds {
        86_400.. => format!("{} j", seconds / 86_400),
        3_600.. => format!("{} h", seconds / 3_600),
        _ => format!("{} min", (seconds / 60).max(1)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn book() -> (People, PathBuf) {
        let directory = std::env::temp_dir().join(format!("x2d-bot-{}-{}", std::process::id(), crate::now()));
        std::fs::create_dir_all(&directory).unwrap();
        (People::new(&directory), directory)
    }

    #[test]
    fn only_the_owner_is_obeyed() {
        let mine =
            json!({ "update_id": 1, "message": { "chat": { "id": 42 }, "from": { "id": 42 }, "text": "/personnes" } });
        let theirs =
            json!({ "update_id": 2, "message": { "chat": { "id": 7 }, "from": { "id": 7 }, "text": "/invite Moi" } });
        let in_a_group =
            json!({ "update_id": 3, "message": { "chat": { "id": 42 }, "from": { "id": 7 }, "text": "/invite Moi" } });
        assert_eq!(from_owner(&mine, 42), Some("/personnes"));
        assert_eq!(from_owner(&theirs, 42), None);
        assert_eq!(from_owner(&in_a_group, 42), None);
    }

    #[test]
    fn invitations_revocations_and_the_list() {
        let (people, directory) = book();
        let reply = answer("/invite Jean Dupont operateur", &people, "https://x2d");
        assert!(reply.starts_with("Voici le lien de Jean Dupont, en tant qu'opérateur"), "{reply}");
        let link = reply.lines().last().unwrap();
        let token = link.strip_prefix("https://x2d/invite/").unwrap();
        assert_eq!(people.invited(token).unwrap().unwrap().role, Role::Operator);
        assert!(answer("/invite@ai_agent230_bot Max", &people, "https://x2d").contains("Max, en tant que spectateur"));
        assert!(answer("/invite", &people, "https://x2d").starts_with("Dis-moi qui inviter"));
        assert!(answer("/invite operateur", &people, "https://x2d").starts_with("Dis-moi qui inviter"));
        let list = answer("/personnes", &people, "");
        assert!(list.contains("• Jean Dupont (opérateur) : aucun navigateur, lien valable encore 7 j"), "{list}");
        assert_eq!(answer("/revoquer Max", &people, ""), "Max n'a plus accès, sur aucun navigateur.");
        assert!(answer("/revoquer Max", &people, "").starts_with("Je ne connais personne"));
        assert!(answer("bonjour", &people, "").starts_with("Je peux"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn durations_read_in_their_largest_unit() {
        assert_eq!(remaining(7 * 86_400 - 5), "6 j");
        assert_eq!(remaining(5 * 3_600 + 10), "5 h");
        assert_eq!(remaining(30), "1 min");
    }
}
