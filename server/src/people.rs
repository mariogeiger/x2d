//! The people invited, what each may do, and the tokens that sign them in.
//!
//! An invitation is a one-time link: whoever opens it and accepts it gets a
//! login token in a cookie, and the invitation is spent. A token is 32 random
//! bytes; only its SHA-256 digest is kept, so a copy of the file signs nobody
//! in. Revoking a person ends their invitation and every login at once.
//!
//! The book lives in `people.json`, shared by the server and the commands
//! that invite: each change happens under an exclusive lock, and lands whole
//! by renaming.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x2d_protocol::Role;

/// Who a request comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// Their name.
    pub name: String,
    /// What they may do.
    pub role: Role,
}

/// A person as listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Their name.
    pub name: String,
    /// What they may do.
    pub role: Role,
    /// Until when their invitation can be accepted, if it is pending.
    pub invited_until: Option<i64>,
    /// How many browsers they are signed in on.
    pub logins: usize,
}

/// The book of people, in the data directory.
#[derive(Clone)]
pub struct People {
    path: PathBuf,
    lock: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Book {
    people: Vec<Person>,
}

#[derive(Serialize, Deserialize)]
struct Person {
    name: String,
    role: Role,
    invitation: Option<Invitation>,
    logins: Vec<Login>,
}

#[derive(Serialize, Deserialize)]
struct Invitation {
    digest: String,
    until: i64,
}

#[derive(Serialize, Deserialize)]
struct Login {
    digest: String,
    since: i64,
}

impl People {
    /// The book kept in `data`.
    pub fn new(data: &Path) -> Self {
        Self { path: data.join("people.json"), lock: data.join("people.lock") }
    }

    /// Invite `name` for `days` days, replacing any invitation they had and
    /// keeping their logins. They come as `role` if given, else as they did
    /// before, or as a spectator if they are new. Return the invitation token
    /// and the role it signs in with.
    pub fn invite(&self, name: &str, role: Option<Role>, days: u32) -> anyhow::Result<(String, Role)> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a person needs a name");
        let token = fresh_token()?;
        let until = crate::now() + i64::from(days) * 86_400;
        let role = self.change(|book| {
            let invitation = Some(Invitation { digest: digest(&token), until });
            Ok(match book.people.iter_mut().find(|person| person.name == name) {
                Some(person) => {
                    person.role = role.unwrap_or(person.role);
                    person.invitation = invitation;
                    person.role
                }
                None => {
                    let role = role.unwrap_or(Role::Spectator);
                    book.people.push(Person { name: name.to_owned(), role, invitation, logins: Vec::new() });
                    role
                }
            })
        })?;
        Ok((token, role))
    }

    /// Who the invitation `token` is for, if it can still be accepted.
    pub fn invited(&self, token: &str) -> anyhow::Result<Option<Signed>> {
        let digest = digest(token);
        let now = crate::now();
        Ok(self.read()?.people.into_iter().find_map(|person| {
            let invitation = person.invitation.as_ref()?;
            (invitation.digest == digest && invitation.until > now)
                .then_some(Signed { name: person.name, role: person.role })
        }))
    }

    /// Spend the invitation `token`: return a login token and who it signs
    /// in, or nothing if the invitation is spent, expired or unknown.
    pub fn accept(&self, token: &str) -> anyhow::Result<Option<(String, Signed)>> {
        let invitation = digest(token);
        let login = fresh_token()?;
        let now = crate::now();
        self.change(|book| {
            let Some(person) = book.people.iter_mut().find(|person| {
                person.invitation.as_ref().is_some_and(|pending| pending.digest == invitation && pending.until > now)
            }) else {
                return Ok(None);
            };
            person.invitation = None;
            person.logins.push(Login { digest: digest(&login), since: now });
            Ok(Some((login.clone(), Signed { name: person.name.clone(), role: person.role })))
        })
    }

    /// Sign `name` in as `role` on one more browser, adding them to the
    /// book if they are new; return the login token.
    pub fn sign_in(&self, name: &str, role: Role) -> anyhow::Result<String> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a person needs a name");
        let login = fresh_token()?;
        let since = crate::now();
        self.change(|book| {
            let person = match book.people.iter().position(|person| person.name == name) {
                Some(index) => &mut book.people[index],
                None => {
                    book.people.push(Person { name: name.to_owned(), role, invitation: None, logins: Vec::new() });
                    book.people.last_mut().expect("just pushed")
                }
            };
            person.role = role;
            person.logins.push(Login { digest: digest(&login), since });
            Ok(())
        })?;
        Ok(login)
    }

    /// Who the login `token` signs in, if anyone.
    pub fn signed_in(&self, token: &str) -> anyhow::Result<Option<Signed>> {
        let digest = digest(token);
        Ok(self.read()?.people.into_iter().find_map(|person| {
            person
                .logins
                .iter()
                .any(|login| login.digest == digest)
                .then(|| Signed { name: person.name.clone(), role: person.role })
        }))
    }

    /// Forget `name`: their invitation and every login. Return whether they
    /// were in the book.
    pub fn revoke(&self, name: &str) -> anyhow::Result<bool> {
        self.change(|book| {
            let before = book.people.len();
            book.people.retain(|person| person.name != name);
            Ok(book.people.len() < before)
        })
    }

    /// Everyone in the book.
    pub fn list(&self) -> anyhow::Result<Vec<Listed>> {
        let now = crate::now();
        Ok(self
            .read()?
            .people
            .into_iter()
            .map(|person| Listed {
                invited_until: person.invitation.map(|invitation| invitation.until).filter(|&until| until > now),
                logins: person.logins.len(),
                name: person.name,
                role: person.role,
            })
            .collect())
    }

    fn locked(&self, exclusive: bool) -> anyhow::Result<File> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)
            .with_context(|| format!("cannot open {}", self.lock.display()))?;
        if exclusive {
            lock.lock()?
        } else {
            lock.lock_shared()?
        }
        Ok(lock)
    }

    fn load(&self) -> anyhow::Result<Book> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(error) => Err(anyhow!("cannot read {}: {error}", self.path.display())),
        }
    }

    fn read(&self) -> anyhow::Result<Book> {
        let _lock = self.locked(false)?;
        self.load()
    }

    fn change<T>(&self, change: impl FnOnce(&mut Book) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let _lock = self.locked(true)?;
        let mut book = self.load()?;
        let result = change(&mut book)?;
        let fresh = self.path.with_extension("json.new");
        std::fs::write(&fresh, serde_json::to_vec_pretty(&book)?)?;
        std::fs::rename(&fresh, &self.path)?;
        Ok(result)
    }
}

/// A fresh token of 256 random bits, in URL-safe base64.
fn fresh_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("no randomness: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The digest a token is kept as, in hexadecimal.
fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> (People, PathBuf) {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        let directory = std::env::temp_dir().join(format!("x2d-people-{}", digest(&format!("{random:?}"))));
        std::fs::create_dir_all(&directory).unwrap();
        (People::new(&directory), directory)
    }

    #[test]
    fn an_invitation_signs_in_once() {
        let (people, directory) = book();
        let invitation = people.invite("Max", None, 7).unwrap().0;
        assert_eq!(people.invited(&invitation).unwrap().unwrap().name, "Max");
        let (login, signed) = people.accept(&invitation).unwrap().unwrap();
        assert_eq!(signed, Signed { name: "Max".into(), role: Role::Spectator });
        assert!(people.accept(&invitation).unwrap().is_none(), "an invitation is spent once accepted");
        assert_eq!(people.signed_in(&login).unwrap(), Some(signed));
        assert_eq!(people.signed_in("forged").unwrap(), None);
        let text = std::fs::read_to_string(directory.join("people.json")).unwrap();
        assert!(!text.contains(&login) && !text.contains(&invitation), "only digests are kept");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn revoking_signs_out_everywhere() {
        let (people, directory) = book();
        let (first, _) = people.accept(&people.invite("Mario", Some(Role::Operator), 7).unwrap().0).unwrap().unwrap();
        let (token, role) = people.invite("Mario", None, 7).unwrap();
        assert_eq!(role, Role::Operator, "a person invited again without a role keeps theirs");
        let (second, _) = people.accept(&token).unwrap().unwrap();
        assert_eq!(people.list().unwrap()[0].logins, 2, "a new invitation keeps the logins");
        assert!(people.revoke("Mario").unwrap());
        assert_eq!(people.signed_in(&first).unwrap(), None);
        assert_eq!(people.signed_in(&second).unwrap(), None);
        assert!(!people.revoke("Mario").unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn signing_in_directly_adds_a_login() {
        let (people, directory) = book();
        let (invitation, _) = people.invite("Ada", None, 7).unwrap();
        let first = people.sign_in("Ada", Role::Operator).unwrap();
        assert_eq!(people.signed_in(&first).unwrap(), Some(Signed { name: "Ada".into(), role: Role::Operator }));
        assert!(people.invited(&invitation).unwrap().is_some(), "a pending invitation stays");
        let second = people.sign_in("Grace", Role::Operator).unwrap();
        assert_eq!(people.signed_in(&second).unwrap().unwrap().name, "Grace");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_expired_invitation_is_refused() {
        let (people, directory) = book();
        let invitation = people.invite("Late", None, 0).unwrap().0;
        assert!(people.invited(&invitation).unwrap().is_none());
        assert!(people.accept(&invitation).unwrap().is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
