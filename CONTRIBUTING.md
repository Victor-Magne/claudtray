# Fluxo de branches

## Branches permanentes

| Branch | Papel |
|---|---|
| `master` | Produção. Última a receber merge — nunca se trabalha diretamente aqui. |
| `dev` | Integração antes de produção. Recebe merge de `test`. |
| `test` | Primeira integração das branches de feature/correção. |
| `hotfix` | Base para correções urgentes de produção. |

## Fluxo normal

```
feature/<nome>  ─┐
fix/<nome>      ─┴──►  test  ──►  dev  ──►  master
```

1. Cria a branch a partir de `test`: `git checkout test && git checkout -b feature/<nome>`.
2. Desenvolve, faz commit.
3. Merge (via PR) para `test`.
4. Quando `test` estiver validada, merge de `test` para `dev`.
5. Quando `dev` estiver pronta para lançar, merge de `dev` para `master`.

Nunca saltar etapas (feature → master direto, ou test → master direto).

## Hotfix

Para correções urgentes em produção, que não podem esperar pelo fluxo normal:

1. Cria a branch a partir de `master`: `git checkout master && git checkout -b hotfix/<nome>`.
2. Corrige, faz commit.
3. Merge para `master` (release imediato).
4. **Depois** do merge para `master`, replica a correção para `dev` e `test` (merge ou cherry-pick) — para não perder a correção na próxima promoção normal.

## Nomenclatura de branches

- `feature/<descrição-curta>` — funcionalidade nova.
- `fix/<descrição-curta>` — correção não urgente (segue o fluxo normal via `test`).
- `hotfix/<descrição-curta>` — correção urgente de produção.

## Nomenclatura de commits

Segue o padrão já usado no histórico do projeto — `<tipo>: <descrição>`, em PT ou EN conforme o que já estiver na mesma área do código:

- `feat:` — funcionalidade nova.
- `fix:` — correção de bug.
- `chore:` — manutenção sem impacto funcional (manifests winget/scoop, bump de versão, dependências).
- `docs:` — apenas documentação (README, CLAUDE.md, este ficheiro).
- `merge:` — merges de promoção entre branches permanentes (`test → dev`, `dev → master`).

Referencia a issue/PR relevante entre parênteses quando existir, ex.: `fix: clippy warnings + CI pipeline (#8)`.

## Antes de abrir PR

Corre localmente exatamente o que o CI (`.github/workflows/ci.yml`) corre — é a única
opinião que conta na altura do merge:

```bash
cargo check
cargo check --target x86_64-pc-windows-gnu
cargo clippy --all-targets -- -D warnings
cargo test
```

O `cargo check --target x86_64-pc-windows-gnu` cross-compila o código partilhado
para Windows a partir do Linux — é fácil esquecer, e é aqui que quebra código
`cfg`-gated que só foi testado num dos dois sistemas operativos (ver CLAUDE.md,
secção "Build & Run").

`cargo fmt --check` não corre no CI de propósito: o código-base ainda não passa
nele, e adicionar um check que falha contra código já existente só ensina a
ignorar CI vermelho.

Se não puderes correr alguma parte (por exemplo, sem acesso a Windows para
testar o instalador manualmente), diz isso no corpo do PR em vez de omitir.
Correr metade e não dizer qual metade é pior do que não correr nada.
