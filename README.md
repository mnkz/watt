# Watt

Compteur de jetons dans la barre des tâches. Windows, fond noir, fenêtre de 400 px.

Claude, Codex, Grok et Cursor sont lus dans les journaux déjà présents sur la machine. Antigravity (AGY) a sa ligne, mais ses fichiers locaux ne contiennent pas de compteurs. Les journaux restent sur le disque : Watt n’en garde que les volumes, le nom du modèle et le prix.

## À l’écran

Cinq périodes : aujourd’hui, hier, 7 jours, 30 jours, ou deux dates. Chaque fournisseur affiche ses jetons et ce que les mêmes appels auraient coûté au tarif public. En dessous, l’abonnement saisi à la main, proratisé sur les jours affichés (`montant × jours / 30`), et le rapport entre les deux.

Un clic sur une ligne détaille les modèles. Échap ou la croix cache la fenêtre. Le clic droit sur l’icône **W** propose Quitter. L’argument `--tray` démarre directement dans la zone de notification, pour le dossier Démarrage.

## D’où viennent les chiffres

| | Source | Comptage |
| --- | --- | --- |
| Claude | `~/.claude/projects/**/*.jsonl` | `usage` de chaque réponse. Lecture de cache et écritures 5 min / 1 h séparées. `stats-cache.json` n’est pas utilisé. |
| Codex | `~/.codex/sessions/**/*.jsonl` | uniquement `token_usage_record`. Les totaux cumulés du fil sont laissés de côté. |
| Grok | `~/.grok/logs/unified.jsonl` | événements `shell.turn.inference_done`, dédoublonnés. |
| Cursor | `%APPDATA%\Cursor\User\globalStorage\state.vscdb` | bulles assistant. La date est l’ouverture de la session. Le modèle n’est pas écrit, le tarif est indicatif. |
| AGY | conversations Antigravity | aucun jeton exploitable. La ligne reste vide. |

Quand un fournisseur inclut déjà le cache dans le prompt, ces jetons ne sont pas additionnés une seconde fois. Le contexte long (Codex au-delà de 272 k, Grok au-delà de 200 k) est tarifé appel par appel. La grille est dans `src/price.rs`, datée du 29 oct. 2026 à l’écran.

Les abonnements ne sont pas détectés. Au premier lancement : Claude Pro 20 $, Codex Plus 20 $, Grok Premium 30 $, AGY offert, Cursor Pro 20 $. On les change dans le panneau. Ils restent dans `%LOCALAPPDATA%\Watt\config.json`.

La première lecture peut traverser plusieurs gigaoctets. La suite est dans `%LOCALAPPDATA%\Watt\cache.json` : seules les lignes nouvelles sont relues, environ toutes les 60 secondes.

## Compiler

Rust édition 2024, Windows.

```
cargo build --release
```

L’exécutable est `target\release\watt.exe`. Une seconde copie se retire toute seule.

Pour l’ouverture de session, un raccourci dans le dossier Démarrage :

```powershell
$s = (New-Object -ComObject WScript.Shell).CreateShortcut(
  "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Watt.lnk")
$s.TargetPath = "$PWD\target\release\watt.exe"
$s.Arguments = "--tray"
$s.WorkingDirectory = "$PWD"
$s.WindowStyle = 7
$s.Save()
```

## Licence

[MIT](LICENSE).
