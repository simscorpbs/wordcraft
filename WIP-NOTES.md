# WIP notes: close like Word, cloud files covered on open, no stale recovery copy, readable Recover buttons

Snapshot taken 2026-10-16 ~17:20 UTC because the PC had to shut down. Nothing here has been compiled or tested yet (by rule, the interns don't run cargo; the coordinating session compiles once at the end).

## Intern session
- Model: Luna max (gpt-6-luna, effort max) via the `estagiario` command on Brenno's PC.
- Session id: `20261016-122758-gpt-6-luna-max-4760`
- Full log: `C:\Users\Brenno\estagiario-logs$2.log`
- Resume (from a worktree on this branch, clean tree): `estagiario continuar 20261016-122758-gpt-6-luna-max-4760 "<what to fix or finish>"`
- Status: stopped on purpose at ~17:25 UTC (PC shutting down) while idle between steps, before its final report. The session is resumable with the command above.

## Done (per the files on disk; not yet reviewed item by item)
- Edits in crates/ui-egui/src/lib.rs, crates/ui-egui/src/backstage.rs, crates/engine/src/recovery.rs and apps/wordcraft/src/main.rs for items A-D of the brief; the intern was in its final static review.

## Missing
- Final report from the intern; confirm every item A-D and its tests against the brief.
- Then: review item by item, translations and docs (Sol), one `cargo xtask ci` (6 Windows-only failures known on base 86adfd34), commit "Cloud AutoSave: Word-like close, cloud files covered on open, version history", push feat/cloud-autosave, merge into fork main.

## Last words in the log (final)
```
As assinaturas confirmam que `file.save` com `ifUnchanged` retorna `saved` ou `conflict` e já atualiza `dirty`/stamp. Vou centralizar a limpeza de recuperação em `Session`, fazer o AutoSave cobrir arquivos abertos dentro de uma raiz cloud, e ajustar o fluxo de fechamento para reaproveitar esse resultado sem duplicar o prompt.
A parte principal já está implementada. Agora estou revisando os caminhos de erro e as interações com os diálogos, especialmente para garantir que um conflito substitua o Save Changes sem perder a ação pendente e que o caso local continue bloqueado. Depois vou formatar e revisar o diff sem compilar.
Vou revisar o diff dos quatro arquivos, conferir os testes adicionados e fazer apenas verificações estáticas permitidas. Não vou compilar nem abrir aplicativos.
A revisão encontrou um ponto estático de portabilidade: o novo helper de recuperação deixava o parâmetro sem uso no wasm, o que pode virar warning no CI. Vou ajustar isso sem alterar o comportamento e depois conferir o diff novamente.
O diff revisado confirma as mudanças nos quatro arquivos autorizados. Vou fazer a última checagem do contexto da branch e do fluxo de `file.save`/CloudRoot; os arquivos locais de planejamento não existem nesta cópia, então registro isso como limite da revisão.
Vou revisar o diff já preparado, conferir as assinaturas e os fluxos de fechamento, recuperação e backstage, e fazer apenas ajustes necessários. Não vou compilar nem executar testes, conforme a instrução.
```

## Brief given
```
# Contexto comum (WordCraft, AutoSave na nuvem, rodada 2)
Repositório Rust: WordCraft, processador de texto open source que busca paridade com o Microsoft Word. Branch base: feat/cloud-autosave (HEAD a678885), que já tem o AutoSave nas pastas do iCloud Drive, Google Drive e OneDrive: crate `crates/cloud` (descoberta das pastas, `DiskStamp` para conflito), comandos do engine `file.cloud` / `file.saveToCloud` / `file.resolveConflict` (crates/engine/src/cmd/cloud.rs), `file.save` com `ifUnchanged`, a guarda de conflito no AutoSave e os diálogos SaveConflict / SaveToCloud na UI (crates/ui-egui/src/dialogs.rs e lib.rs). Veja `git diff 86adfd34 HEAD` e `docs/` para o contexto.
O objetivo desta rodada é que o AutoSave se comporte como o Word com o OneDrive.
Regras do repositório (leia o CLAUDE.md da raiz e siga-o): nenhum unwrap/expect/panic/unreachable fora de teste, erros por Result, nada de unsafe, número vindo de arquivo ou parâmetro é hostil (get(), aritmética checada, limites). Tudo o que o usuário vê é comando (CommandSpec) com teste. Nada abaixo da camada L6 depende de egui. Não quebre o wasm (código de disco e thread fica atrás de #[cfg(not(target_arch = "wasm32"))], como o resto do repo faz). Cores vêm de theme::Tokens. Textos de interface em inglês dentro de tl!(...); as traduções outro agente faz depois, não mexa em crates/ui-egui/src/i18n/*.tsv nem em docs/ ou ROADMAP.md.
PROIBIDO rodar cargo build, check, clippy ou test, e qualquer compilação: o PC não aguenta compilações paralelas e quem compila sou eu, depois. Só `cargo fmt` é permitido. Escreva o código e os testes com cuidado redobrado com tipos e empréstimos, lendo as assinaturas reais no código antes de usar qualquer função.
Trabalhe SOMENTE nos arquivos listados na sua tarefa. Não faça commit. No fim, liste cada arquivo alterado e o que mudou, os testes novos (nome e o que provam), e tudo o que você ficou em dúvida se compila.
# Sua tarefa: fechar como o Word, arquivo da nuvem coberto ao abrir, recuperação limpa, botões legíveis
Pasta: C:\Users\Brenno\wc-a. Arquivos seus: crates/ui-egui/src/lib.rs (só as partes de fechar/Save Changes/AutoSave), crates/ui-egui/src/backstage.rs, crates/ui-egui/src/dialogs.rs (só se precisar), crates/engine/src/recovery.rs, crates/engine/src/session.rs (só se precisar), apps/wordcraft/src/main.rs e os testes desses arquivos. Outro agente está criando o histórico de versões ao mesmo tempo em outra pasta (crates/engine/src/cmd/tools.rs `file.versions`, um painel novo na UI): não mexa nisso, e mude o mínimo possível fora do que é seu para o merge ficar limpo.

A. Fechar com AutoSave ligado não pergunta. Hoje, fechar a janela com o AutoSave ligado pergunta "Do you want to save changes?". O Word nunca pergunta por um arquivo com AutoSave. Em crates/ui-egui/src/lib.rs, em `close_requested` e nos caminhos que abrem o Dialog::SaveChanges antes de file.close / New / Open: quando `self.autosaves()` e o documento está sujo, grave na hora com `self.session.run("file.save", &json!({"ifUnchanged": true}))`. Gravou (`saved` true): segue sem perguntar (fecha, abre o novo etc.). Voltou `conflict`: chama `self.show_save_conflict()` e cancela a ação. Erro: cai no comportamento atual (pergunta). E se um Dialog::SaveChanges estiver aberto e o documento deixar de estar sujo (o AutoSave chegou antes), feche o diálogo e deixe a ação pendente (`then`/`params`) rodar.
B. Arquivo aberto de uma pasta da nuvem fica coberto na hora. Hoje, `autosave_block` devolve NotSavedHere até a pessoa salvar uma vez. Pule o NotSavedHere quando `wordcraft_engine::cloud::locate(path, &self.session.cloud.roots)` for Some e `self.session.cloud.stamp` for Some (ajuste `autosave_path` de acordo, para que `saved_here()` e o resto continuem coerentes). Arquivo local fora da nuvem fica com a regra atual. Confira o que acontece com formatos que o AutoSave não regrava (o bloqueio Format continua valendo).
C. Documento salvo não deixa cópia de recuperação. Depois de reabrir, File > Recover ofereceu uma cópia de um documento que o AutoSave já tinha salvo. Em crates/engine/src/recovery.rs, crie um método público de Session que apaga as gerações de recuperação do documento atual quando ele não está sujo, reaproveitando a limpeza de `clear_recovery_before_password` (só os registros com `r.document_is(self.document_id())`). Chame esse método quando o app fecha com o documento salvo (`close_requested` devolvendo true; o caminho do Cmd+Q / encerrar em apps/wordcraft/src/main.rs, antes de terminar) e logo depois do flush do AutoSave no fechamento (item A).
D. Botões em branco na página Recover. Em crates/ui-egui/src/backstage.rs, `recovery_page` faz `ui.visuals_mut().override_text_color = Some(egui::Color32::WHITE)` (perto da linha 162), e o texto dos botões fica branco sobre branco. Tire isso e use as cores do tema (theme::Tokens, veja como as outras páginas do backstage fazem), legível no tema claro e no escuro. Procure o mesmo override em outras páginas e corrija igual.
Testes (em Rust, ao lado dos existentes, por exemplo os de autosave em lib.rs e os de backstage.rs): fechar com AutoSave e documento sujo grava e fecha sem diálogo; fechar com conflito abre o SaveConflict e não fecha; SaveChanges aberto some quando o documento fica limpo e a ação pendente roda; arquivo aberto de uma pasta da nuvem (use um CloudRoot apontando para uma pasta temporária, como os testes de crates/engine/src/cmd/cloud.rs fazem) é coberto pelo AutoSave sem salvar antes, e um arquivo local fora da nuvem continua bloqueado; documento salvo não deixa geração de recuperação (habilite a recuperação numa pasta temporária, como os testes existentes de recovery fazem); a página Recover não usa override de cor branca.
```
