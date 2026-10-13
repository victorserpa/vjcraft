# Formato .avc (composição) — especificação para importador

Fontes: `default/Example.avc` (v7.3.3), `~/Documents/Resolume Arena/Compositions/Example.avc` (v7.15.0), `rest/docs/swagger.yaml`. Nenhum binário aberto. Ressalva: presets não lidos; swagger lido parcialmente (Composition, ChoiceParameter, RangeParameter, lista de schemas).

## 1. Árvore XML
- **Composition**: name, uniqueId (u64, ms), numDecks, currentDeckIndex, numLayers, numColumns, compositionIsRelative (0/1, paths relativos — inferido)
  - versionInfo (name, major/minor/microVersion, revision)
  - CompositionInfo (name, description, width, height) > DeckInfo (name, id, closed)
  - Params name="Params"|"Dashboard" > Param / ParamRange / ParamChoice / ParamColor / ParamText
  - CompositionView / LayerView / ClipView > FoldParams > FoldState — só UI
  - Notes > Note (text, text_size_multiplier, text_color_index) — 7.15
  - AudioTrack > AudioEffectChain, Params, PrimarySource > AudioFileSource (FileName)
  - VideoTrack > Params, RenderPass (RenderPassChain), ChoosableMixer (Blend | Blend mode | Transition; manualDuration em Clip)
    - RenderPass: storage, name, type, uniqueTypeId, uniqueId, baseType ∈ {RenderPassChain, Effect, DryWetEffect, Mixer, Generator}, index, dwType. Filhos: View, Params, ChoosableMixer (dry/wet), AnimationSettings > Setting (name, flags), WireRenderPass > SliceInputs
  - Layer (uniqueId, layerIndex) > Params Dashboard, ClipTransition, LayerView, AudioTrack, VideoTrack
  - Column (uniqueId, columnIndex, name) > Params
  - CrossFader > AudioTrack, VideoTrack
  - TempoController > Params (Tempo)
  - Deck (name, uniqueId, closed, numLayersWithContent, numColumnsWithContent, numLayers, numColumns, deckIndex)
    - Params, ColumnAttributes (index, name), Column
    - Clip (name, uniqueId, layerIndex, columnIndex)
      - PreloadData > EmbeddedThumbnail (LZF, w, h, 32bpp, base64), VideoFile, AudioFile
      - Params, ClipView, Transport > Params
      - VideoTrack > PrimarySource > VideoSource (storage, name, w, h, type ∈ {GeneratorVideoSource, CompositionRouterVideoSource}) > RenderPass Generator | VideoFormatReaderSource (fileName)
      - AudioTrack

## 2. Parâmetros
- **Param**: T ∈ {BOOL, STRING}; name, default, value, altName
- **ParamRange** (T=DOUBLE): ValueRange "minMax" (min,max); PhaseSource*: Static(phase), Timeline(phase, defaultMillisecondsDuration), TransportTimeline(defaultBeatsDuration), DashboardLink(linkId, linkName); Beats_d (mode, numDetectedBeats, numManualBeats, detectedTempo, manualTempo, detected); DurationSource; BehaviourDouble > Modifier > ModifierEnvelope > points > point(x,y,curve)
- **ParamChoice**: T ∈ {INT32, UINT8, UINT64, COLOR}, storeChoices; Choice (name, value, enabled, active, shortName). Blend Mode = UINT64 id de mixer (tabela não está no arquivo)
- **ParamColor**: ARGB u32 decimal; channelmode, paletteEnabled, interpolated; Params Channels, ColorPalette
- **ParamText**: STRING + Params
- Identidade do param = `name` dentro do Params do pai; doubles com 20 casas → f64

## 3. Mídia, efeitos, tempo
- Mídia: path absoluto em VideoFormatReaderSource@fileName / AudioFileSource@FileName / PreloadData. Resolver: absoluto → relativo ao .avc → busca por nome
- Generator: RenderPass baseType=Generator em GeneratorVideoSource
- Efeito: RenderPass Effect/DryWetEffect por uniqueTypeId
- Blend: ChoosableMixer > RenderPass Mixer + ParamChoice "Blend Mode"
- Transição: ClipTransition + ChoosableMixer "Transition"
- BPM: TempoController/Tempo; por clip Beats_d.manualTempo
- Dashboard: Link 1..N via PhaseSourceDashboardLink
- Outputs: não estão no .avc (provável Preferences/)

## 4. .avc ↔ swagger
| .avc | swagger |
|---|---|
| Composition | Composition |
| Deck | Deck |
| Column / ColumnAttributes | Column |
| Layer | Layer |
| Clip | Clip, VideoTrackClip, AudioTrackClip |
| RenderPass Effect | VideoEffect, Effect |
| RenderPass Generator / VideoSource | Source |
| VideoFormatReaderSource | VideoFileInfo |
| ClipTransition | ClipTransition, LayerTransition |
| CrossFader | CrossFader |
| TempoController | TempoController |
| Transport | TransportTimeline, TransportBPMSync |
| Param | BooleanParameter, StringParameter |
| ParamRange | RangeParameter |
| ParamChoice | ChoiceParameter |
| ParamColor | ColorParameter |
| ParamText | TextParameter |

## 5. Modelo Rust
```rust
struct Composition { name: String, width: u32, height: u32, version: (u32,u32,u32,u32), relative_paths: bool, tempo: f64, decks: Vec<Deck>, layers: Vec<Layer>, columns: Vec<Column>, master: Track, dashboard: Vec<Param> }
struct Deck { id: u64, name: String, index: u32, clips: Vec<Clip>, column_names: BTreeMap<u32,String> }
struct Layer { id: u64, index: u32, track: Track, transition: Option<Transition> }
struct Clip { id: u64, name: String, layer: u32, column: u32, source: Source, track: Track, transport: Vec<Param>, thumbnail: Option<Vec<u8>> }
enum Source { File{path: PathBuf}, Generator(Plugin), Router, AudioFile{path: PathBuf} }
struct Track { params: Vec<Param>, effects: Vec<Plugin>, blend: Option<Plugin> }
struct Plugin { type_id: String, kind: String, name: String, params: Vec<Param>, mixer: Option<Box<Plugin>> }
struct Param { name: String, value: Value, default: Value, range: Option<(f64,f64)>, anim: Option<Anim> }
enum Value { Bool(bool), Str(String), F64(f64), Choice{v:u64, opts: Vec<(String,u64)>}, Color(u32) }
enum Anim { Static(f64), TimelineMs(f64), Beats{beats:f64}, DashLink(String), Envelope(Vec<(f64,f64,u32)>) }
```
Importador: quick-xml → árvore genérica → passe tipado; guardar XML cru do não suportado.
- MVP 1:1: Composition, Deck, Layer, Column, Clip, mídia de arquivo, Opacity, Blend Mode, Tempo, params com range
- Ignorar no MVP (com aviso): FoldState, View, AnimationSettings, Notes, Wire, EmbeddedThumbnail, CompositionRouter, envelopes, efeitos FFGL proprietários

## 6. Incertezas
compositionIsRelative, storage, flags, curve, Beats_d.mode; tabela de ids de Blend Mode; LZF = liblzf?; semântica de phase; local dos Outputs; tags ausentes nos exemplos (LayerGroup, efeitos de áudio).
