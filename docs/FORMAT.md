# Formato do repositório Cofre — versão 1

Este documento é o contrato do formato em disco. Qualquer mudança incompatível exige incrementar `format_version` (config) ou `SNAPSHOT_VERSION` (snapshots) e escrever uma migração.

## Layout

```
config                     JSON em texto claro
data/<ab>/<id>             um objeto por bloco, <ab> = 2 primeiros caracteres do id
snapshots/<id>             um objeto por snapshot
locks/<id>                 travas de operações em andamento
```

Nomes de objetos são hex minúsculo (2 a 128 caracteres). Arquivos começando com `.` (ex.: `.tmp-*`) são temporários e ignorados pela listagem; o `prune` remove os que sobraram.

Nenhum objeto é sobrescrito: `put` falha se o nome já existe.

## `config`

```json
{
  "format_version": 1,
  "repo_id": "<16 bytes hex>",
  "created": 1767268800,
  "kdf": { "algorithm": "argon2id-v19", "memory_kib": 65536, "iterations": 3, "parallelism": 1, "salt": "<16 bytes hex>" },
  "chunker": { "algorithm": "fastcdc2020", "min_size": 262144, "avg_size": 1048576, "max_size": 4194304 },
  "compression_level": 3,
  "keys": "<envelope hex>"
}
```

`keys` é um envelope (ver abaixo) cifrado com a KEK = Argon2id(senha, salt), AAD `cofre:keys:v1`, contendo 64 bytes: `enc_key (32) || id_key (32)`.

Trocar a senha só exige regravar `keys`; os dados não mudam.

## Envelope criptografado

```
versão (1 byte = 0x01) || nonce (24 bytes) || ciphertext || tag Poly1305 (16 bytes)
```

Cifra: XChaCha20-Poly1305 com `enc_key`. Nonce aleatório de 192 bits por objeto.

AAD por tipo, para impedir que um objeto seja trocado por outro:

| Objeto     | AAD                                   |
|------------|---------------------------------------|
| bloco      | `cofre:data:` + 32 bytes do id        |
| snapshot   | `cofre:snapshot:` + id em hex (ASCII) |
| trava      | `cofre:lock:` + id em hex (ASCII)     |

## Blocos (`data/`)

- Id = BLAKE3 em modo keyed com `id_key` sobre o conteúdo original. Sem a chave, o destino não consegue testar se um arquivo conhecido está no backup.
- Texto claro do envelope: `codificação (1 byte) || payload`, onde `0x00` = bruto e `0x01` = zstd. Usa-se bruto quando a compressão não reduz o tamanho.
- Na leitura: abrir envelope → descomprimir (limite `max_size`) → recalcular id → comparar. Qualquer divergência é erro, nunca dado silencioso.

## Snapshots (`snapshots/`)

Id = 32 bytes aleatórios em hex. Texto claro do envelope = zstd de:

```
SNAPSHOT_VERSION (u32 little-endian = 1) || postcard(Snapshot)
```

```
Snapshot { time: i64, hostname: String, paths: Vec<String>, tags: Vec<String>, nodes: Vec<Node>, summary: Summary }
Node     { path: String, kind: NodeKind, mode: u32, mtime: i64, mtime_nanos: u32 }
NodeKind = Dir | File { size: u64, chunks: Vec<[u8; 32]> } | Symlink { target: String }
Summary  { files, dirs, symlinks, bytes, new_chunks, new_bytes_stored, warnings, duration_ms: u64 }
```

`path` é o caminho absoluto normalizado, sem raiz, separado por `/` (`C:\x\y` vira `C/x/y`). Na restauração, componentes vazios, `.`, `..`, `\`, `:` e NUL são rejeitados.

A ordem dos campos é a ordem do postcard; postcard não é autodescritivo, por isso o número de versão vem antes do corpo.

## Travas (`locks/`)

Envelope com JSON `{ exclusive, hostname, pid, time }`. Backup e forget usam trava compartilhada; prune usa exclusiva. Uma trava é renovada a cada 5 minutos e considerada abandonada após 30 minutos sem renovação.

## Garantias de consistência

1. Blocos são gravados antes do snapshot que os referencia; o snapshot é o último objeto escrito. Backup interrompido deixa apenas blocos órfãos, que o `prune` remove.
2. Escrita local: arquivo temporário → `fsync` → link/rename → `fsync` do diretório.
3. `prune` aborta se qualquer snapshot não puder ser lido, para nunca apagar bloco em uso.
4. Servidor append-only recusa `DELETE` (exceto em `locks/`) e qualquer sobrescrita.

## Histórico

- v1 (2026-10): versão inicial.
