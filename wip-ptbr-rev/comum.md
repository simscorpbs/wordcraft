# Revisão da tradução pt-BR da interface do WordCraft

## O que é
O WordCraft é um processador de texto em Rust que imita o Microsoft Word. A interface é traduzida
por catálogos TSV. O catálogo do português do Brasil é
`C:\Users\Brenno\wordcraft\crates\ui-egui\src\i18n\pt-br.tsv` (formato no cabeçalho dele:
`contexto<TAB>inglês<TAB>tradução`; `\n`, `\t` e `\\` são escapes; `{nome}` tem de aparecer
igual nas duas colunas; reticências `…` finais no inglês exigem `…` na tradução).
O usuário, brasileiro, usa o Word em português do Brasil todo dia e achou traduções erradas.

Você NÃO edita o repositório. Você lê o seu trecho e grava as propostas na sua pasta de trabalho.
Pode ler o código do repositório para ver onde e como cada texto aparece (`rg -n "texto" C:\Users\Brenno\wordcraft\crates`).

## O que procurar, entrada por entrada
1. **Termo diferente do Word pt-BR.** O nome de recurso, aba, grupo, botão, menu, diálogo ou
   barra de status deve ser o que o Word em português do Brasil mostra. Nomes de recurso podem
   seguir o Word; frases (mensagens, dicas, descrições) são redação nossa, a partir do sentido do
   inglês, sem copiar frase da Microsoft.
2. **Erro de português:** gramática, concordância, regência, crase, acento, hífen, ortografia,
   português de Portugal (ficheiro, ecrã, utilizador, guardar no sentido de salvar, transferir no
   sentido de baixar, apagar onde o Word diz excluir), tradução literal que soa estrangeira.
3. **Texto que ficou em inglês** na coluna de tradução, salvo nome próprio de produto ou formato
   (WordCraft, PDF, Markdown, MCP, Zotero, APA, Unicode, Rust, WASM).
4. **Texto que não cabe.** Rótulo de aba, botão da faixa, item de barra de status e coluna de
   tabela muito mais longo que o inglês (mais de ~40% maior) ou que o Word: proponha forma curta
   que o Word usaria. Veja no código se o texto é rótulo da faixa (`ribbon`), dica (tooltip) ou frase.
5. **Sentido errado:** tradução que não diz o que o inglês diz no contexto do código (ex.: "Draft"
   como modo de exibição é "Rascunho"; "Break" no Layout é "Quebra"; "Track Changes" é "Controlar
   Alterações"). Confirme o contexto no código quando houver dúvida.
6. **Inconsistência:** o mesmo termo inglês traduzido de dois jeitos em entradas diferentes.

## Maiúsculas
Decisão do projeto: abas, grupos da faixa, botões e itens de menu (nomes de recurso) seguem a
capitalização do Word pt-BR, com iniciais maiúsculas nas palavras principais e minúsculas em
artigo, preposição e conjunção ("Aceitar Todas as Alterações", "Pincel de Formatação",
"Inserir Nota de Rodapé"). Frase, mensagem, dica e descrição ficam com só a primeira letra
maiúscula. Só proponha troca de maiúscula onde a entrada for claramente nome de recurso.

## Vocabulário de referência (nomes de recurso do Word pt-BR)
Abas: Arquivo, Página Inicial, Inserir, Desenhar, Design, Layout, Referências, Correspondências,
Revisão, Exibir, Desenvolvedor, Ajuda. Contextuais: Design da Tabela, Layout (da tabela), Formato
da Forma, Formato da Imagem, Cabeçalho e Rodapé, Equação.
Página Inicial: Área de Transferência (Colar, Recortar, Copiar, Pincel de Formatação), Fonte
(Negrito, Itálico, Sublinhado, Tachado, Subscrito, Sobrescrito, Cor do Realce do Texto, Cor da
Fonte, Limpar Toda a Formatação, Maiúsculas e Minúsculas, Aumentar/Diminuir Tamanho da Fonte),
Parágrafo (Marcadores, Numeração, Lista de Vários Níveis, Diminuir/Aumentar Recuo, Classificar,
Mostrar Tudo, Alinhar à Esquerda, Centralizar, Alinhar à Direita, Justificar, Espaçamento de
Linha e Parágrafo, Sombreamento, Bordas), Estilos, Edição (Localizar, Substituir, Selecionar).
Inserir: Folha de Rosto, Página em Branco, Quebra de Página, Tabela, Imagens, Formas, Ícones,
Modelos 3D, SmartArt, Gráfico, Instantâneo, Link, Indicador, Referência Cruzada, Comentário,
Cabeçalho, Rodapé, Número de Página, Caixa de Texto, Partes Rápidas, WordArt, Letra Capitular,
Linha de Assinatura, Data e Hora, Objeto, Equação, Símbolo.
Layout: Margens, Orientação, Tamanho, Colunas, Quebras, Números de Linha, Hifenização, Recuo,
Espaçamento, Posição, Quebra Automática de Texto, Avançar, Recuar, Painel de Seleção, Alinhar,
Agrupar, Girar. Design: Temas, Cores, Fontes, Espaçamento entre Parágrafos, Efeitos, Marca
d'Água, Cor da Página, Bordas de Página.
Referências: Sumário, Adicionar Texto, Atualizar Sumário, Inserir Nota de Rodapé, Inserir Nota
de Fim, Próxima Nota de Rodapé, Mostrar Notas, Inserir Citação, Gerenciar Fontes Bibliográficas,
Estilo, Bibliografia, Inserir Legenda, Inserir Índice de Ilustrações, Marcar Entrada, Inserir
Índice, Marcar Citação, Índice de Autoridades.
Correspondências: Envelopes, Etiquetas, Iniciar Mala Direta, Selecionar Destinatários, Editar
Lista de Destinatários, Bloco de Endereço, Linha de Saudação, Inserir Campo de Mesclagem,
Visualizar Resultados, Concluir e Mesclar.
Revisão: Ortografia e Gramática, Dicionário de Sinônimos, Contar Palavras, Ler em Voz Alta,
Traduzir, Idioma, Novo Comentário, Excluir, Anterior, Próximo, Controlar Alterações, Marcação
Simples, Toda a Marcação, Sem Marcação, Original, Mostrar Marcação, Painel de Revisão, Aceitar,
Rejeitar, Comparar, Combinar, Restringir Edição.
Exibir: Modo de Leitura, Layout de Impressão, Layout da Web, Estrutura de Tópicos, Rascunho,
Foco, Leitura Avançada, Régua, Linhas de Grade, Painel de Navegação, Zoom, 100%, Uma Página,
Várias Páginas, Largura da Página, Nova Janela, Organizar Tudo, Dividir, Lado a Lado, Rolagem
Sincronizada, Macros.
Arquivo (Backstage): Início, Novo, Abrir, Informações, Salvar, Salvar como, Imprimir,
Compartilhar, Exportar, Fechar, Conta, Opções, Recentes, Documento em branco, Recuperar
Documentos Não Salvos, Proteger Documento, Inspecionar Documento, Gerenciar Documento.
Gerais: Salvamento Automático, Desfazer, Refazer, Barra de Ferramentas de Acesso Rápido,
Faixa de Opções, Recolher a Faixa de Opções, Pesquisar, Página X de Y, palavras, caracteres,
Revisor de Texto, Configurar Página, Parágrafo, Fonte (diálogo), Hiperlink/Link, Senha,
Somente leitura, OK, Cancelar, Aplicar, Fechar, Ajuda.
Diálogos e termos: Recuo, Antes, Depois, Entre linhas, Simples, 1,5 linha, Duplo, Pelo menos,
Exatamente, Múltiplos; Retrato, Paisagem; Superior, Inferior, Esquerda, Direita, Medianiz;
Margens espelho; Cabeçalho diferente na primeira página; Quebra de seção (Próxima Página,
Contínua, Página Par, Página Ímpar); Mesclar Células, Dividir Células, Linha de Cabeçalho,
Linhas em Tiras, Inserir Acima/Abaixo/à Esquerda/à Direita; Fórmula; Classificar.
Use isso como referência de nome; o que não estiver aqui, decida pelo uso consagrado do Word
pt-BR que você conhece, e na dúvida diga "dúvida" no motivo.

## O que entregar
Na sua pasta de trabalho, o arquivo `propostas.tsv`, UTF-8, uma linha por entrada que muda
(não liste as que estão certas), colunas separadas por TAB:
`linha<TAB>contexto<TAB>inglês<TAB>tradução atual<TAB>proposta<TAB>categoria<TAB>motivo`
- `linha` é o número que vem na primeira coluna do seu trecho (vazio para entrada nova).
- `inglês` copiado exatamente como está, com os escapes.
- `categoria`: termo-word, gramatica, ingles, nao-cabe, sentido, consistencia, maiuscula, nova.
- `motivo`: uma frase curta em português, dizendo onde o texto aparece quando isso pesar.
A proposta respeita os escapes, os `{placeholders}` e as reticências finais.

## Critério de pronto
Antes de encerrar, rode a checagem `python C:\Users\Brenno\ptbr-rev\checar.py <sua pasta>\propostas.tsv`
e ela tem de sair com `0 problemas`. Todas as entradas do seu trecho foram lidas. No relatório
final diga quantas entradas leu, quantas propostas fez por categoria e as que ficaram em dúvida.
