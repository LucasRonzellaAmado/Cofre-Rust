# Cofre

Backup incremental, deduplicado e criptografado de ponta a ponta, em Rust.

- Blocos por conteúdo (FastCDC): mudou o meio do arquivo, só sobem os blocos afetados.
- Tudo é cifrado na máquina antes de sair (XChaCha20-Poly1305, chave derivada com Argon2id). O destino não vê nomes nem conteúdo.
- Compressão zstd por bloco.
- Destinos: pasta local, servidor Cofre (HTTP, modo append-only anti-ransomware) e S3 compatível.
- Restauração de arquivo, pasta ou snapshot inteiro, em qualquer caminho.
- Retenção (`--keep-daily 7 --keep-weekly 4 --keep-monthly 12`) com limpeza segura de blocos órfãos.
- `check --read-data` prova que os backups restauram.
- Heartbeat para alerta de backup que falhou ou não rodou.

## Estrutura

```
crates/cofre-core     formato, chunking, cripto, snapshots, backup/restore/check/prune
crates/cofre-cli      binário `cofre` + destinos HTTP e S3
crates/cofre-server   binário `cofre-server` (axum)
docs/FORMAT.md        especificação do formato v1
deploy/               Docker Compose, Nginx, systemd, agendador do Windows
```

## Uso rápido

```bash
cargo build --release
export COFRE_REPO=/mnt/backup/cofre
cofre init
cofre backup ~/Documentos --exclude node_modules --exclude '*.tmp'
cofre snapshots
cofre ls latest
cofre restore latest --target /tmp/volta --include /home/lucas/Documentos/contrato.pdf
cofre check --read-data
cofre forget --keep-daily 7 --keep-weekly 4 --keep-monthly 12 --prune
```

Senha: `COFRE_PASSWORD`, `COFRE_PASSWORD_FILE` ou digitada no terminal.

Códigos de saída: `0` ok, `1` erro, `3` concluído com avisos (arquivo ilegível ou alterado durante o backup).

## Destinos

| `--repo`                         | Destino                                                                 |
|----------------------------------|-------------------------------------------------------------------------|
| `/caminho` ou `local:/caminho`   | disco local                                                             |
| `https://cofre.exemplo.com`      | `cofre-server` (token em `COFRE_SERVER_TOKEN` ou `COFRE_SERVER_TOKEN_FILE`) |
| `s3:bucket/prefixo`              | S3 / MinIO / Backblaze B2 (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`, `AWS_ENDPOINT`) |

## Servidor no homelab

```bash
cd deploy
openssl rand -hex 32 > cofre_token.txt
docker compose up -d --build
```

Coloque `deploy/nginx-cofre.conf` no Nginx com TLS. O compose sobe em modo append-only: clientes adicionam backups, mas não apagam nem sobrescrevem. A limpeza (`forget`/`prune`) é feita pelo administrador direto no volume:

```bash
COFRE_REPO=/var/lib/docker/volumes/deploy_cofre-data/_data cofre forget --keep-daily 7 --keep-weekly 4 --keep-monthly 12 --prune
```

## Agendamento e alertas

- Linux: `deploy/cofre-backup.service` + `deploy/cofre-backup.timer` (`systemctl enable --now cofre-backup.timer`).
- Windows: `deploy/agendar-windows.ps1`.
- `COFRE_HEARTBEAT_URL` (healthchecks.io ou compatível): o Cofre chama `/start`, a URL base no sucesso e `/fail` na falha. Se nenhuma chamada chegar no prazo, o serviço avisa.

## Desenvolvimento

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench -p cofre-core
cargo deny check
```
