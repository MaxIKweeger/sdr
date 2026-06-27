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

## Interface web

Une interface web Rust/WASM (Leptos + Axum) est accessible depuis n'importe quel navigateur :

| Connexion | Adresse |
|-----------|---------|
| Câble (réseau local) | http://192.168.1.123:8080 |
| WiFi hotspot « SDR-Station » | http://192.168.2.22:8080 |

**Se connecter via le hotspot** : WiFi → réseau **SDR-Station** → mot de passe **darty92220** → ouvrir http://192.168.2.22:8080

L'interface affiche :
- Statut GPS (coordonnées en temps réel quand fix)
- Prochain passage satellite (nom, heure, élévation, fréquence)
- Planning 48 h de tous les passages NOAA/Meteor
- Galerie des images capturées avec lightbox

Le serveur web (`sdr-web.service`) démarre automatiquement au boot.
Sources dans `web/` (workspace Cargo séparé : backend Axum + frontend Leptos WASM).

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

## Réseau

| Interface | Mode | Adresse | Usage |
|-----------|------|---------|-------|
| eth0 | filaire (IP fixe) | 192.168.1.123 | Connexion à la box / internet |
| wlan0 | Hotspot WiFi AP | 192.168.2.22 | Accès direct sans box (terrain) |

- **Câble** : connecte le Pi à la box → internet disponible, IP fixe `192.168.1.123`.
- **Hotspot** : SSID `SDR-Station`, canal 6 (2.4 GHz), WPA2, mot de passe `darty92220`.
  Les clients reçoivent une IP en `192.168.2.x` via DHCP et accèdent à l'interface web.
- Les deux interfaces fonctionnent **simultanément**.
- SSH : `ssh hugues@192.168.1.123` (câble) ou `ssh hugues@192.168.2.22` (WiFi hotspot).

## Fonctionnement HORS-LIGNE

- **TLE** (`tle_cache/`) : lancer `./target/release/sdr passes` UNE FOIS avec internet
  avant de partir (orbites valables ~1-2 semaines). Hors-ligne → repli auto sur ce cache.
- **Position** : fournie par le GPS, aucune connexion requise.
- **Interface web** : fonctionne hors-ligne via le hotspot SDR-Station.

## Antenne (dipôle V, satellites 137 MHz)

- 2 brins de **~52 cm** chacun, à plat (horizontaux), angle **~120°**, plan orienté **Nord-Sud**.
- Vue dégagée sur le ciel (dehors / point haut). Viser les passages d'élévation > 30-40°.

## Services systemd

| Service | Rôle | Commande |
|---------|------|----------|
| `sdr.service` | Capture automatique (`sdr auto`) | `sudo systemctl start\|stop\|status sdr` |
| `sdr-web.service` | Interface web port 8080 | `sudo systemctl start\|stop\|status sdr-web` |
| `gpsd.service` | Démon GPS (/dev/ttyACM0) | `sudo systemctl start\|stop\|status gpsd` |

⚠️ `sdr.service` monopolise la clé SDR : pour un usage manuel, d'abord `sudo systemctl stop sdr`.

Logs en direct : `journalctl -u sdr -f` / `journalctl -u sdr-web -f`

## Matériel / système

- Raspberry Pi 5 8 Go, Debian 13 trixie (aarch64), boot sur NVMe SSD (adaptateur PCIe).
- Clé SDR : Nooelec NESDR SMArt v5 (RTL2832U, 0x0bda).
- GPS : u-blox VK-172 sur /dev/ttyACM0, géré par gpsd.
- SatDump 1.2.2 compilé depuis les sources → `/usr/bin/satdump`.
- Driver TV noyau blacklisté ; accès USB via `plugdev` ; accès série GPS via `dialout`.

## Recompiler après modif des sources

```bash
source ~/.cargo/env
# Programme SDR principal
cd /home/hugues/sdr && cargo build --release
# Interface web (backend)
cd /home/hugues/sdr/web && cargo build -p backend --release
# Interface web (frontend WASM)
cd /home/hugues/sdr/web/frontend && trunk build --release
```
