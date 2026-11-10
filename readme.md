# Projeto: Discord Screen Share Companion

**Documento:** Especificação inicial do produto e arquitetura  
**Versão:** 0.1  
**Status:** Planejamento do MVP

---

# 1. Visão geral

## 1.1. Objetivo

Desenvolver um aplicativo desktop independente que permita aos usuários compartilhar suas telas com outras pessoas que estejam em uma chamada do Discord.

O aplicativo funcionará como um complemento ao Discord, sem substituir suas funcionalidades de comunicação.

O Discord continuará sendo utilizado para chamadas de voz e comunicação. O novo aplicativo será responsável exclusivamente pela transmissão e recepção de vídeo.

## 1.2. Problema que queremos resolver

O compartilhamento de tela nativo do Discord pode apresentar limitações de qualidade, desempenho ou compatibilidade para determinados usuários.

A proposta é oferecer uma alternativa leve, simples e independente, com transmissão P2P.

## 1.3. Princípios do produto

- Interface simples, com o mínimo de interações possível.
- Aplicativo leve, executado em segundo plano.
- Autenticação exclusivamente via Discord.
- Comunicação de vídeo preferencialmente P2P.
- Nenhuma necessidade de criar uma conta própria.
- O Discord permanece como ambiente principal de comunicação.
- Compatibilidade inicial prioritária com Windows.

---

# 2. Experiência do usuário

## 2.1. Instalação

Ao instalar o aplicativo:

1. O usuário instala o programa normalmente.
2. O aplicativo fica disponível na bandeja do sistema (System Tray).
3. O usuário pode configurar sua inicialização automática com o Windows.
4. Na primeira execução, é apresentada a autenticação com Discord.

O aplicativo deve poder permanecer aberto em segundo plano sem manter uma janela visível.

## 2.2. Primeiro acesso e autenticação

A tela inicial deve apresentar:

**Conectar com Discord**

O processo de autenticação deverá:

1. Abrir o navegador padrão.
2. Redirecionar o usuário para o OAuth2 do Discord.
3. Solicitar autorização do escopo `identify`.
4. Receber o retorno da autenticação.
5. Vincular a sessão ao usuário.
6. Armazenar a sessão de forma segura.

Informações utilizadas:

- Discord User ID.
- Username.
- Global Name.
- Avatar.

Não solicitar permissões desnecessárias.

Não armazenar o Client Secret do Discord no aplicativo desktop.

A autenticação deverá ser implementada com fluxo seguro de OAuth2, validação de `state` e proteção contra interceptação do callback. Avaliar PKCE conforme o fluxo suportado pela configuração da aplicação Discord.

## 2.3. Transmitir tela

O usuário deverá conseguir iniciar uma transmissão por três caminhos:

- Atalho global configurável.
- Opção "Transmitir tela" na bandeja do sistema.
- Botão "Transmitir minha tela" na interface principal.

**Importante:** não assumir que será possível interceptar ou substituir o botão nativo de transmissão do Discord. Essa integração não deve ser requisito do MVP.

Fluxo:

1. Usuário aciona a opção de transmissão.
2. Aplicativo abre a janela de configuração.
3. Usuário seleciona monitor ou janela.
4. Usuário confirma a transmissão.
5. Aplicativo inicia a captura.
6. Uma sala é criada.
7. Um código de conexão é gerado.
8. O código é exibido ao transmissor.

## 2.4. Assistir a uma transmissão

Os demais participantes:

1. Abrem o aplicativo pela bandeja do sistema.
2. Selecionam "Conectar a uma transmissão".
3. Informam o código recebido.
4. O aplicativo valida o código.
5. O participante entra na sala.
6. A transmissão é exibida em uma janela própria.

O espectador deverá conseguir sair da transmissão sem encerrar a sessão dos demais participantes.

## 2.5. Bandeja do sistema

O menu da bandeja deve oferecer:

- Abrir aplicativo.
- Transmitir minha tela.
- Conectar a uma transmissão.
- Mostrar status da conexão.
- Configurações.
- Sair do aplicativo.

---

# 3. Escopo do MVP

## 3.1. Funcionalidades obrigatórias

- [ ] Aplicativo desktop instalável.
- [ ] Execução em segundo plano.
- [ ] Ícone na bandeja do sistema.
- [ ] Inicialização automática configurável.
- [ ] Login via Discord.
- [ ] Identificação do usuário autenticado.
- [ ] Seleção de monitor ou janela.
- [ ] Captura de tela.
- [ ] Transmissão de vídeo via WebRTC.
- [ ] Criação automática de sala.
- [ ] Geração de código de conexão.
- [ ] Entrada em sala por código.
- [ ] Exibição do vídeo recebido.
- [ ] Encerramento de transmissão.
- [ ] Desconexão individual de espectadores.
- [ ] Limite inicial de quatro espectadores por transmissão.

## 3.2. Fora do MVP

Não implementar inicialmente:

- Transmissão de áudio.
- Chat próprio.
- Compartilhamento de arquivos.
- Gravação de transmissões.
- Aplicativo mobile.
- Versão web.
- Transmissão simultânea de múltiplas telas.
- Sistema próprio de amigos.
- Integração profunda com canais de voz do Discord.
- Transcodificação de vídeo em servidor.

---

# 4. Requisitos de qualidade

Configuração inicial desejada:

| Parâmetro | Meta inicial |
|---|---|
| Resolução | 1280 × 720 |
| FPS | 30 |
| Bitrate | 2,5 Mbps |
| Codec | H.264 preferencialmente |
| Espectadores | Até 4 |
| Transporte | WebRTC |
| Topologia | P2P |
| Áudio | Fora do MVP |

Os parâmetros deverão ser configuráveis internamente para testes.

A aplicação deve priorizar baixo consumo de CPU e memória.

Sempre que possível, utilizar codificação e decodificação por hardware.

Não confundir tamanho do instalador com consumo de memória durante uma transmissão. Ambos deverão ser medidos separadamente.

---

# 5. Arquitetura técnica

## 5.1. Stack proposta

### Aplicativo desktop

- Rust.
- Tauri 2.
- React.
- TypeScript.

O Tauri será responsável pela interface e integração com o sistema operacional.

O Rust será responsável pela lógica de aplicação, gerenciamento de sessões e integração com os componentes nativos.

### Comunicação de vídeo

- WebRTC.
- ICE.
- STUN.
- TURN como fallback.
- libdatachannel como candidata para implementação do transporte WebRTC.

A escolha definitiva da biblioteca WebRTC deverá depender de um protótipo funcional.

### Backend

- Rust.
- Axum.
- WebSocket.
- API HTTP para autenticação e gerenciamento de salas.

### Autenticação

- Discord OAuth2.
- Escopo `identify`.

### Persistência

O MVP não exige banco de dados tradicional.

As salas podem ser mantidas em memória no servidor de sinalização, com expiração automática.

Não armazenar conteúdo de vídeo no backend.

---

# 6. Arquitetura de comunicação

## 6.1. Visão geral

A arquitetura será dividida em três componentes:

1. Aplicativo desktop.
2. Servidor de sinalização.
3. Comunicação WebRTC P2P.

O servidor de sinalização não deverá transportar o vídeo.

Sua função será permitir que os participantes se encontrem e negociem a conexão.

## 6.2. Fluxo de conexão

1. O transmissor autentica-se.
2. O transmissor solicita a criação de uma sala.
3. O backend gera um código temporário.
4. O espectador informa esse código.
5. O backend valida o acesso e associa o espectador à sala.
6. Os participantes trocam mensagens SDP e candidatos ICE através do servidor.
7. O WebRTC estabelece a conexão.
8. O vídeo passa diretamente do transmissor para cada espectador.

## 6.3. Topologia P2P

Utilizar inicialmente uma topologia Mesh.

O transmissor estabelece uma conexão WebRTC individual com cada espectador.

Exemplo:

- Transmissor → Espectador 1.
- Transmissor → Espectador 2.
- Transmissor → Espectador 3.
- Transmissor → Espectador 4.

O transmissor precisará enviar uma cópia do fluxo para cada participante.

Com bitrate de 2,5 Mbps:

| Espectadores | Upload aproximado |
|---|---:|
| 1 | 2,5 Mbps |
| 2 | 5 Mbps |
| 3 | 7,5 Mbps |
| 4 | 10 Mbps |

Valores sem considerar overhead de rede.

O limite de espectadores deverá ser aplicado no backend.

## 6.4. STUN e TURN

STUN será utilizado para descoberta de caminhos de conexão.

TURN deverá existir como fallback quando a conexão P2P direta não for possível.

Importante:

- STUN não transporta vídeo.
- O servidor de sinalização não transporta vídeo.
- TURN retransmite mídia quando necessário.
- O uso de TURN pode gerar custos de tráfego.

O aplicativo deverá funcionar sem TURN em cenários onde a conexão direta seja possível, mas a arquitetura não deverá depender exclusivamente disso.

---

# 7. Servidor de sinalização

## 7.1. Responsabilidades

O backend será responsável por:

- Validar sessões autenticadas.
- Criar salas.
- Gerar códigos de acesso.
- Controlar participantes.
- Impedir excesso de espectadores.
- Gerenciar expiração de salas.
- Encaminhar mensagens de sinalização WebRTC.
- Notificar desconexões.

Não deverá processar, gravar ou armazenar vídeo.

## 7.2. Modelo de sala

Estrutura conceitual:

```json
{
  "room_id": "uuid",
  "room_code": "A7K9P2",
  "host_user_id": "discord_user_id",
  "status": "active",
  "created_at": "timestamp",
  "expires_at": "timestamp",
  "max_viewers": 4,
  "viewers": []
}
```

O código deve ser aleatório, não sequencial, com entropia adequada e validação no backend.

Requisitos:

- Código temporário.
- Expiração automática.
- Código invalidado ao encerrar a transmissão.
- Limite de tentativas de conexão.
- Limite de espectadores.
- Um transmissor por sala.

## 7.3. Eventos WebSocket

Eventos conceituais:

```text
room.create
room.created

room.join
room.joined
room.viewer_joined
room.viewer_left

webrtc.offer
webrtc.answer
webrtc.ice_candidate

room.end
room.ended

error
```

As mensagens devem incluir identificadores de sala, remetente e destinatário quando aplicável.

O backend deverá validar se o remetente pertence à sala e se tem permissão para enviar cada tipo de evento.

---

# 8. Captura e processamento de vídeo

## 8.1. Captura

O usuário deverá poder selecionar:

- Monitor principal.
- Monitores secundários.
- Janelas abertas.

A captura deverá utilizar APIs nativas sempre que isso oferecer melhor desempenho e compatibilidade.

## 8.2. Codificação

Preferir H.264 quando disponível e compatível.

Priorizar:

- Hardware encoding.
- Baixa latência.
- Controle de bitrate.
- Controle de FPS.
- Redução de resolução quando necessário.

Evitar introduzir FFmpeg no pipeline sem necessidade comprovada.

## 8.3. Decodificação

O espectador deverá receber e renderizar o vídeo com o menor atraso possível.

A aplicação deverá apresentar estados claros:

- Conectando.
- Conectado.
- Reconectando.
- Transmissão encerrada.
- Falha de conexão.

---

# 9. Segurança

Requisitos mínimos:

- Comunicação HTTPS e WSS.
- Tokens de autenticação armazenados com proteção adequada ao sistema operacional.
- Não incluir secrets no frontend ou binário desktop.
- Validar todas as permissões no backend.
- Impedir entrada em salas encerradas.
- Limitar tentativas de adivinhação de códigos.
- Aplicar expiração automática de salas.
- Validar tamanho e formato das mensagens WebSocket.
- Não confiar em IDs enviados pelo cliente sem validação de sessão.
- Permitir que o transmissor remova participantes ou encerre a sala.

O código da sala não deve ser tratado como substituto da autenticação do usuário.

---

# 10. Interface e identidade visual

A interface deverá ser minimalista.

## 10.1. Tela principal

Elementos:

- Identidade Discord do usuário.
- Status da conexão.
- Botão "Transmitir minha tela".
- Botão "Conectar a uma transmissão".
- Acesso às configurações.

## 10.2. Tela de transmissão

Elementos:

- Lista de monitores e janelas disponíveis.
- Pré-visualização, se viável.
- Resolução.
- FPS.
- Botão iniciar transmissão.

Após iniciar:

- Código da sala.
- Quantidade de espectadores.
- Status da conexão.
- Botão encerrar transmissão.

## 10.3. Tela de conexão

Elementos:

- Campo para código.
- Botão conectar.
- Estado da conexão.
- Mensagens de erro compreensíveis.

## 10.4. Janela do espectador

Elementos:

- Vídeo recebido.
- Nome do transmissor.
- Status da conexão.
- Botão desconectar.
- Opção de tela cheia.

---

# 11. Estrutura sugerida do projeto

```text
discord-screen-companion/
│
├── apps/
│   ├── desktop/
│   │   ├── src/
│   │   │   ├── components/
│   │   │   ├── screens/
│   │   │   ├── hooks/
│   │   │   ├── services/
│   │   │   └── types/
│   │   │
│   │   └── src-tauri/
│   │       ├── src/
│   │       ├── capabilities/
│   │       └── tauri.conf.json
│   │
│   └── signaling-server/
│       └── src/
│           ├── auth/
│           ├── rooms/
│           ├── signaling/
│           ├── websocket/
│           └── main.rs
│
├── packages/
│   └── shared/
│       └── protocols/
│
├── docs/
│   ├── ARCHITECTURE.md
│   ├── PROTOCOL.md
│   ├── SECURITY.md
│   └── DEVELOPMENT.md
│
└── README.md
```

A estrutura poderá ser adaptada conforme as necessidades reais da implementação.

---

# 12. Plano de desenvolvimento

## Fase 1 — Prova de conceito técnica

**Objetivo:** validar os componentes de maior risco antes de construir o produto.

Implementar:

- Aplicativo Tauri.
- Captura de monitor.
- Captura de janela.
- Transporte WebRTC.
- Transmissão entre duas instâncias do aplicativo.
- Renderização do vídeo recebido.
- Teste de codificação H.264.

Critério de conclusão:

Duas instâncias conseguem estabelecer uma conexão e transmitir vídeo 720p a 30 FPS em uma rede compatível.

**Gate técnico:** validar a compatibilidade da WebView do Tauri e das APIs nativas de captura. Não presumir que `getDisplayMedia` funcione de forma idêntica em Windows, macOS e Linux.

Validar também a maturidade e integração real do binding Rust escolhido para libdatachannel. Se necessário, utilizar integração FFI com a biblioteca C/C++.

## Fase 2 — Backend de sinalização

Implementar:

- Servidor Axum.
- WebSocket.
- Criação de salas.
- Geração de códigos.
- Entrada por código.
- Troca de SDP.
- Troca de ICE candidates.
- Controle de participantes.
- Expiração de salas.

Critério de conclusão:

Dois computadores conseguem se encontrar através do código e estabelecer conexão WebRTC.

## Fase 3 — Autenticação Discord

Implementar:

- OAuth2.
- Callback de autenticação.
- Identificação do usuário.
- Gerenciamento seguro de sessão.
- Logout.

Critério de conclusão:

O usuário consegue autenticar-se pelo Discord e acessar as funcionalidades protegidas.

## Fase 4 — Integração com a experiência desktop

Implementar:

- System Tray.
- Inicialização automática.
- Menu da bandeja.
- Atalho global.
- Janela de transmissão.
- Janela do espectador.
- Gerenciamento de estados.

Critério de conclusão:

O usuário consegue iniciar e assistir transmissões sem precisar manter a janela principal aberta.

## Fase 5 — Estabilidade e otimização

Implementar:

- Reconexão.
- Controle de bitrate.
- Controle de FPS.
- Limite de espectadores.
- Tratamento de erros.
- Métricas de conexão.
- Testes de consumo de CPU e RAM.

Critério de conclusão:

O aplicativo mantém uma transmissão estável em cenários reais de uso, dentro dos requisitos definidos.

## Fase 6 — Distribuição

Implementar:

- Instalador para Windows.
- Ícone e identidade visual.
- Atualização do aplicativo.
- Configuração de inicialização automática.
- Assinatura do executável, quando disponível.
- Documentação de instalação.

---

# 13. Critérios de aceitação do MVP

O MVP será considerado funcional quando:

1. O usuário conseguir instalar e abrir o aplicativo.
2. O aplicativo conseguir permanecer na bandeja do sistema.
3. O usuário conseguir autenticar-se via Discord.
4. O transmissor conseguir selecionar uma tela ou janela.
5. O aplicativo conseguir criar uma sala e gerar um código.
6. Outro usuário conseguir entrar usando esse código.
7. O vídeo for transmitido via WebRTC.
8. A transmissão funcionar sem passar por um servidor de vídeo quando houver conectividade P2P direta.
9. Até quatro espectadores conseguirem participar.
10. O transmissor conseguir encerrar a sessão.
11. Um espectador conseguir sair sem desconectar os demais.
12. O aplicativo tratar corretamente falhas e desconexões.

---

# 14. Diretrizes para o Code Agent

Estas diretrizes devem ser seguidas durante o desenvolvimento:

1. Não implementar todas as funcionalidades de uma vez.
2. Seguir as fases de desenvolvimento na ordem definida.
3. Priorizar provas de conceito para tecnologias ainda não validadas.
4. Não assumir compatibilidade de APIs entre sistemas operacionais sem testes.
5. Manter separação clara entre frontend, lógica Rust, backend e transporte WebRTC.
6. Não colocar regras críticas de autorização exclusivamente no frontend.
7. Não adicionar dependências pesadas sem justificativa técnica.
8. Priorizar baixo consumo de CPU, RAM e largura de banda.
9. Escrever testes para regras de negócio e protocolos.
10. Documentar decisões arquiteturais relevantes.
11. Não implementar recursos fora do escopo sem aprovação.
12. Ao concluir cada fase, apresentar arquivos criados, decisões técnicas, testes executados e limitações conhecidas.

## Primeira tarefa do agente

Não comece implementando o aplicativo completo.

Primeiro:

1. Analise esta especificação.
2. Proponha uma arquitetura executável para Windows.
3. Avalie Tauri 2, Rust e a integração com libdatachannel.
4. Identifique riscos de compatibilidade na captura de tela e codificação de vídeo.
5. Proponha a menor prova de conceito capaz de transmitir vídeo entre duas instâncias.
6. Apresente a estrutura inicial do repositório e as dependências necessárias.

Aguarde validação da arquitetura antes de avançar para as demais fases.

---

# 15. Decisões técnicas ainda abertas

As decisões abaixo deverão ser validadas por protótipos:

- Biblioteca definitiva para WebRTC.
- Estratégia de captura nativa no Windows.
- Estratégia de codificação H.264.
- Integração entre Rust e o pipeline de vídeo.
- Necessidade e provedor de TURN.
- Estratégia de atualização automática.
- Suporte futuro para macOS e Linux.

**Prioridade absoluta:** entregar primeiro uma transmissão funcional entre dois computadores. A interface, autenticação e funcionalidades adicionais devem ser construídas em torno de um pipeline de vídeo comprovadamente funcional.
