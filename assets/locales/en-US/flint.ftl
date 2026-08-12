flint-commit = Commit
flint-version = Version
flint-confirm-quit = Are you sure you want to quit?
flint-quit = Quit
flint-open-url-hint = Paste a URL to open.
flint-invalid-url = Invalid URL: { $error }
flint-code-actions = Code Actions
flint-no-code-actions = No Code Actions Available
flint-selection-controls = Selection Controls
flint-editor-controls = Editor Controls
flint-inline-diagnostics-unavailable = Inline diagnostics are not available until regular diagnostics are enabled.
flint-inline-diagnostics = Inline Diagnostics
flint-inlay-hints = Inlay Hints
flint-semantic-highlights = Semantic Highlights
flint-code-lens = Code Lens
flint-minimap = Minimap
flint-diagnostics = Diagnostics
flint-line-numbers = Line Numbers
flint-selection-menu = Selection Menu
flint-auto-signature-help = Auto Signature Help
flint-inline-git-blame = Inline Git Blame
flint-column-git-blame = Column Git Blame
flint-vim-mode = Vim Mode
flint-helix-mode = Helix Mode
flint-repl-menu = REPL Menu
flint-repl-kernel = Kernel: { $name } ({ $language })
flint-start-repl-for = Start REPL for { $kernel }
flint-setup-repl-for = Set up Flint REPL for { $language }
flint-run-selection = Run Selection
flint-run-line = Run Line
flint-view-sessions = View Sessions
flint-next-hunk = Next Hunk
flint-previous-hunk = Previous Hunk
flint-inotify-title = Could not start inotify
flint-inotify-detail = inotify_init returned { $error }
    
    This may be due to system-wide limits on inotify instances. For troubleshooting, see: https://github.com/shenghsi/flint/blob/main/docs/src/linux.md
flint-windows-watcher-title = Could not start ReadDirectoryChangesW
flint-windows-watcher-detail = ReadDirectoryChangesW initialization failed: { $error }
    
    This may occur on network filesystems and WSL paths. For troubleshooting, see: https://github.com/shenghsi/flint/blob/main/docs/src/windows.md
flint-troubleshoot-and-quit = Troubleshoot and Quit
flint-unsupported-gpu-title = Unsupported GPU
flint-unsupported-gpu-detail = Flint uses { $graphics_api } for rendering and requires a compatible GPU.
    
    You are using a software-emulated GPU ({ $device_name }), which will result in poor performance.
    
    For troubleshooting, see: { $docs_url }
    Set ZED_ALLOW_EMULATED_GPU=1 to override permanently.
flint-skip = Skip
flint-preview-markdown = Preview Markdown
flint-preview-svg = Preview SVG
flint-preview-csv = Preview CSV
flint-preview-open-split = { $shortcut } to open in a split
