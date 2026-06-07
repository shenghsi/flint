## ADDED Requirements

### Requirement: Markdown can open in editable rendered mode
Markdown files SHALL support an editable rendered mode that displays formatted
Markdown inline while preserving the source buffer as the source of truth.

#### Scenario: User opens a Markdown file
- **WHEN** a Markdown file is opened with editable rendered mode enabled
- **THEN** headings, emphasis, lists, quotes, links, code blocks, and other
  supported Markdown structures render inline in the editor surface

#### Scenario: User switches to source view
- **WHEN** the user switches from editable rendered mode to source view
- **THEN** the underlying Markdown source text is available without data loss

### Requirement: Core editing behavior remains editor quality
Editable rendered mode SHALL preserve cursor movement, selection, copy, paste,
undo, redo, search, and save behavior.

#### Scenario: User edits rendered Markdown
- **WHEN** the user edits text in editable rendered mode
- **THEN** the source buffer updates consistently and the edit participates in
  normal undo and redo

#### Scenario: User searches rendered Markdown
- **WHEN** the user searches inside a Markdown document in editable rendered
  mode
- **THEN** matching content can be found and navigated without switching to a
  separate preview pane

### Requirement: Rich Markdown blocks render inline
Editable rendered mode SHALL render common rich Markdown blocks inline where the
existing Markdown rendering stack supports them.

#### Scenario: Markdown contains code and Mermaid blocks
- **WHEN** a Markdown document contains fenced code blocks and Mermaid blocks
- **THEN** the editor displays them as inline rendered blocks while preserving
  editable source semantics

#### Scenario: Markdown contains images and links
- **WHEN** a Markdown document contains images and links
- **THEN** the editor displays appropriate inline affordances while preserving
  the source text

### Requirement: Split preview remains optional
The existing Markdown preview workflow SHALL remain available as an optional
view for users who prefer split or separate preview.

#### Scenario: User opens Markdown preview
- **WHEN** the user invokes the Markdown preview action
- **THEN** the preview opens without requiring editable rendered mode to be
  disabled globally
