flint-commit = 提交
flint-version = 版本
flint-confirm-quit = 确定要退出吗？
flint-quit = 退出
flint-open-url-hint = 粘贴要打开的 URL。
flint-invalid-url = 无效的 URL：{ $error }
flint-code-actions = 代码操作
flint-no-code-actions = 没有可用的代码操作
flint-selection-controls = 选择控制
flint-editor-controls = 编辑器控制
flint-inline-diagnostics-unavailable = 启用常规诊断后才能使用内联诊断。
flint-inline-diagnostics = 内联诊断
flint-inlay-hints = 内嵌提示
flint-semantic-highlights = 语义高亮
flint-code-lens = 代码镜头
flint-minimap = 迷你地图
flint-diagnostics = 诊断
flint-line-numbers = 行号
flint-selection-menu = 选择菜单
flint-auto-signature-help = 自动签名帮助
flint-inline-git-blame = 内联 Git 追责信息
flint-column-git-blame = 列 Git 追责信息
flint-vim-mode = Vim 模式
flint-helix-mode = Helix 模式
flint-repl-menu = REPL 菜单
flint-repl-kernel = 内核：{ $name }（{ $language }）
flint-start-repl-for = 为 { $kernel } 启动 REPL
flint-setup-repl-for = 为 { $language } 设置 Flint REPL
flint-run-selection = 运行所选内容
flint-run-line = 运行当前行
flint-view-sessions = 查看会话
flint-next-hunk = 下一个更改块
flint-previous-hunk = 上一个更改块
flint-inotify-title = 无法启动 inotify
flint-inotify-detail = inotify_init 返回 { $error }
    
    这可能是因为系统范围的 inotify 实例数量限制。故障排除说明请参阅：https://github.com/shenghsi/flint/blob/main/docs/src/linux.md
flint-windows-watcher-title = 无法启动 ReadDirectoryChangesW
flint-windows-watcher-detail = ReadDirectoryChangesW 初始化失败：{ $error }
    
    这可能发生在网络文件系统和 WSL 路径中。故障排除说明请参阅：https://github.com/shenghsi/flint/blob/main/docs/src/windows.md
flint-troubleshoot-and-quit = 故障排除并退出
flint-unsupported-gpu-title = 不支持的 GPU
flint-unsupported-gpu-detail = Flint 使用 { $graphics_api } 进行渲染，需要兼容的 GPU。
    
    当前正在使用软件模拟 GPU（{ $device_name }），这会导致性能很差。
    
    故障排除说明请参阅：{ $docs_url }
    设置 ZED_ALLOW_EMULATED_GPU=1 可永久覆盖此限制。
flint-skip = 跳过
flint-preview-markdown = 预览 Markdown
flint-preview-svg = 预览 SVG
flint-preview-csv = 预览 CSV
flint-preview-open-split = 使用 { $shortcut } 在拆分视图中打开
