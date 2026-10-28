# x2d

La page en direct d'une imprimante Bambu Lab sur le réseau local : sa caméra,
son état, ses températures sur 24 h, son AMS et ses alertes, avec la lumière,
la pause, la reprise et l'arrêt pour les opérateurs. Elle tourne pour une X2D
sur <https://x2d.geiger.ink>, sur invitation seulement.

## Comment ça marche

- `x2d serve` tient une seule connexion MQTT (TLS, port 8883) à l'imprimante.
  Il fusionne ses rapports `push_status` en un état, qu'il traduit en un
  `Printer` lisible.
- La caméra ne s'ouvre (RTSPS, port 322) que si quelqu'un regarde. Le serveur
  dépaquette le H.264 en images et les relaie sans les réencoder à toutes les
  pages ouvertes, par WebSocket. Chaque page les décode avec WebCodecs.
- Le TLS est vérifié : la chaîne doit remonter aux autorités de Bambu Lab
  (`server/certs/bambu.pem`), et le nom commun doit être le numéro de série.
- L'aperçu du plateau vient du `.3mf` sur la carte SD, lu par FTPS (port 990).
  Les textes des alertes HMS viennent du service de Bambu Lab, en français.
- Un bot Telegram signale la fin d'une impression, son échec et chaque
  nouvelle alerte. Il obéit aussi aux commandes du seul chat de la
  configuration : `/invite Max` (ou `/invite Max operateur`) répond par un
  lien d'invitation, `/revoquer Max` retire l'accès, `/personnes` dit qui
  est invité, `/etat` dit où en est l'imprimante et `/photo` montre ce que
  voit la caméra. Sans rôle, une personne invitée à nouveau garde le sien.
  Les messages des autres restent sans réponse. Le bouton de menu du chat
  ouvre la page comme Mini App : le serveur vérifie la signature que
  Telegram donne aux données de lancement, et connecte ainsi le seul
  propriétaire du chat, comme opérateur, sans lien.
- L'accès passe par un lien d'invitation à usage unique. L'accepter pose un
  cookie de connexion `HttpOnly`. Seuls les condensats SHA-256 des jetons sont
  gardés, et `x2d revoke` coupe tous les navigateurs d'une personne.
- Les rôles sont _opérateur_ et _spectateur_. Le serveur vérifie le rôle et
  l'état de l'imprimante avant chaque commande, et note chaque commande dans
  `commands.jsonl`.

## Le code

- `protocol/` : ce que le serveur et la page se disent (état, historique,
  commandes, vidéo).
- `server/` : le binaire `x2d`.
- `client/` : la page Leptos en wasm, construite par `trunk`.

```
cargo test                                   # protocole et serveur
x2d --config x2d.toml check --pictures /tmp  # joindre l'imprimante une fois
x2d --config x2d.toml invite Max             # un lien pour un spectateur
x2d --config x2d.toml invite Mario --role operateur
x2d --config x2d.toml people                 # qui est invité
x2d --config x2d.toml revoke Max
~/git/geiger-server/deploy x2d             # publier sur x2d.geiger.ink
```

La configuration (TOML, hors du dépôt, lisible par son seul propriétaire)
donne l'adresse d'écoute et l'origine publique. Si systemd lui passe une
socket (activation par socket), le serveur l'écoute à la place de l'adresse. Elle donne aussi le dossier
des données, celui de la page construite, et l'imprimante (`host`, `serial`,
`access_code`). Le bot `[telegram]` (`token`, `chat`) est facultatif.
L'imprimante doit avoir le mode développeur pour accepter les commandes, et
la vue en direct LAN pour la caméra.

En production, x2d tourne dans un conteneur du compte `geiger` (voir
`~/git/geiger-server`) : il sert la socket que systemd lui passe, derrière
Caddy, avec la configuration en secret Podman (`/run/secrets/x2d.toml`), ses
données dans `/data` et la page dans `/site`. `ffmpeg` décode les photos
jointes aux notifications. Les invitations s'y font avec la même
configuration : `ssh geiger@localhost podman exec systemd-x2d x2d invite Max`.
