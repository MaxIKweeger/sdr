# Récepteur SDR (RTL-SDR) — Raspberry Pi 5

Récepteur tout-en-Rust pour clé **Nooelec NESDR SMArt v5** (RTL2832U + R820T2).
NOAA (APT analogique) décodé en Rust ; Meteor-M (LRPT numérique) décodé via SatDump.

## L'essentiel (à lire en premier)

Pour capter la météo, **tout est automatique, une seule commande** :

```bash
cd /home/hugues/sdr
./target/release/sdr auto
```

Pré-requis : antenne dipôle dehors (vue ciel) + clé SDR branchée + GPS branché.
Le programme attend les bons passages, capture et décode tout seul → dossier `passages/`.
La position vient du GPS, l'heure/les orbites se gèrent toutes seules.

👉 Pour que ça tourne **sans clavier ni écran**, laisse le service s'en charger au boot
(section « Service autonome »). **Tu n'as rien d'autre à régler** : aucune option n'est requise.

Tout ce qui suit est **optionnel** (autres modes, réglages avancés).

## Lancer

```bash
cd /home/hugues/sdr
./target/release/sdr <commande>
```

| Commande | Effet |
|----------|-------|
| `auto`                | 🛰️ autonome : capture chaque bon passage NOAA/Meteor → `passages/` |
| `passes`              | liste les passages satellites (48 h) |
| `noaa <MHz> [s]`      | capture manuelle d'un NOAA → `noaa.bmp` |
| `meteor <MHz> [s]`    | capture Meteor-M (LRPT) → `meteo/` (via SatDump) |
| `fm [MHz]`            | radio FM → `fm.wav` |
| `adsb`                | récepteur ADS-B (avions) |
| `scan`                | balayage de puissance (test du tuner) |

## Position (prédiction des passages) — via GPS

La position de l'observateur est **lue automatiquement sur le GPS USB** (récepteur
u-blox VK-172 sur `/dev/ttyACM0`) au lancement des modes `passes` et `auto`.

- Le GPS doit avoir une **vue dégagée du ciel** (extérieur) pour accrocher un fix.
- Si aucun GPS n'est branché / pas de fix : repli sur **Bagneux** par défaut.
- Périphérique surchargeable : `SDR_GPS=/dev/ttyACM1 ./target/release/sdr passes`.

## Réseau / internet

- **eth0** (filaire) : connexion prioritaire (route metric 100).
- **wlan0** : client WiFi du téléphone — SSID `Thorgal`, auto-connexion (metric 600 = secours).
  → internet via le filaire s'il est là, sinon via le téléphone, automatiquement.
- Gérer le WiFi : `nmcli connection up Thorgal` / `nmcli device status`.
- (L'ancien point d'accès « ProxyPhone » a été désactivé ; sauvegardes en `*.ap-bak`/`*.disabled`.)

## Fonctionnement HORS-LIGNE

- **TLE** (`tle_cache/`) : lancer `./target/release/sdr passes` UNE FOIS avec internet
  avant de partir (orbites valables ~1-2 semaines). Hors-ligne → repli auto sur ce cache.
  (Astuce : active le partage de connexion du téléphone pour rafraîchir les TLE sur le terrain.)
- **Position** : fournie par le GPS, aucune connexion requise.

## Antenne (dipôle V, satellites 137 MHz)

- 2 brins de **~52 cm** chacun, à plat (horizontaux), angle **~120°**, plan orienté **Nord-Sud**.
- Vue dégagée sur le ciel (dehors / point haut). Viser les passages d'élévation > 30-40°.

## Service autonome (démarrage au boot)

Le service systemd `sdr.service` lance `sdr auto` au démarrage.
```bash
sudo systemctl start|stop|status sdr     # gérer
journalctl -u sdr -f                      # suivre les logs
```
⚠️ Le service monopolise la clé SDR : pour un usage manuel, d'abord `sudo systemctl stop sdr`.

## Matériel / système (configuré le 2026-06-21)

- Raspberry Pi 5 8 Go, Debian 13 trixie (aarch64).
- Driver TV noyau blacklisté ; accès USB via `plugdev` ; accès série GPS via `dialout`.
- SatDump 1.2.2 compilé depuis les sources → `/usr/bin/satdump` (sources dans `~/SatDump`).
- Réseau : eth0 filaire (prioritaire) + wlan0 client WiFi du tél « Thorgal » (secours). Voir section Réseau.

## Recompiler après modif des sources

```bash
source ~/.cargo/env
cd /home/hugues/sdr && cargo build --release
```
