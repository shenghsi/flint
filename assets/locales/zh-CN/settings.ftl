settings-language-title = 语言
settings-language-description = 选择 Flint 界面使用的语言。
settings-edit-json = 在 settings.json 中编辑
settings-reset-default = 恢复默认值
settings-copy-link = 复制链接
settings-view-other-projects = 查看其他项目
settings-change-scope = 更改范围
settings-no-results = 无结果
settings-no-results-detail = 没有与“{ $query }”匹配的设置
settings-fix-json = 在 settings.json 中修复
settings-load-failed = 无法加载设置。部分值可能不正确，且更改可能丢失。
settings-outdated = 设置已过期，必须更新。
settings-migrate-automatic = Flint 可以自动将其迁移到最新版本。
settings-migrate-manual = 必须手动将其迁移到最新版本。
settings-migration-failed = 设置文件已过期，且自动迁移失败。
settings-restricted-mode = 受限模式
settings-restricted-detail = 此项目处于受限模式。部分项目设置可能不生效。
settings-manage-trust = 管理信任
settings-search-fonts = 搜索字体…
settings-search-themes = 搜索主题…
settings-search-icon-themes = 搜索图标主题…
settings-feature-enabled-all = 对所有用户启用
settings-feature-reset = 重置
settings-modified-in = —  已在{ $scope }中修改
settings-language-native-name = 简体中文
settings-window-title = Flint — 设置
settings-configure = 配置
settings-search-placeholder = 搜索设置…
settings-focus-content = 聚焦内容
settings-focus-navbar = 聚焦导航栏
settings-scope = 范围
settings-default-section = 设置
settings-general-section-general = 常规设置
settings-general-section-security = 安全
settings-general-section-workspace-restoration = 工作区恢复
settings-general-section-scoped-settings = 范围设置
settings-general-auto-update-title = 自动更新
settings-general-auto-update-description = 是否自动检查更新。
settings-general-when-closing-with-no-tabs-title = 关闭且无标签页时
settings-general-when-closing-with-no-tabs-description = 使用“关闭活动项”操作且没有标签页时的处理方式。
settings-general-on-last-window-closed-title = 关闭最后一个窗口时
settings-general-on-last-window-closed-description = 关闭最后一个窗口时的处理方式。
settings-general-use-system-path-prompts-title = 使用系统路径对话框
settings-general-use-system-path-prompts-description = 为“打开”和“另存为”使用原生操作系统对话框。
settings-general-use-system-prompts-title = 使用系统提示框
settings-general-use-system-prompts-description = 为确认操作使用原生操作系统对话框。
settings-general-redact-private-values-title = 隐藏私密值
settings-general-redact-private-values-description = 隐藏私密文件中变量的值。
settings-general-private-files-title = 私密文件
settings-general-private-files-description = 用于匹配文件路径以确定文件是否为私密文件的通配符规则。
settings-general-cli-default-open-behavior-title = 命令行默认打开行为
settings-general-cli-default-open-behavior-description = 未指定标志时，`flint <path>` 打开目录的方式。
settings-general-trust-all-projects-title = 默认信任所有项目
settings-general-trust-all-projects-description = 打开 Flint 时自动信任所有项目以避免进入受限模式，无需为每个新项目单独授权即可使用全部功能。
settings-general-restore-unsaved-buffers-title = 恢复未保存的缓冲区
settings-general-restore-unsaved-buffers-description = 重启后是否恢复未保存的缓冲区。
settings-general-restore-on-startup-title = 启动时恢复
settings-general-restore-on-startup-description = 打开 Flint 时从上一次会话恢复的内容。
settings-general-preview-channel-title = 预览版设置
settings-general-preview-channel-description = 仅在 Flint 预览版中启用的设置。
settings-general-settings-profiles-title = 设置配置文件
settings-general-settings-profiles-description = 可临时叠加在现有用户设置之上的任意数量的设置配置文件。
settings-source = { $source ->
    [Agent] 智能体
    [Appearance] 外观
    [Collaboration] 协作
    [Editor] 编辑器
    [Extensions] 扩展
    [General] 常规
    [Git] Git
    [Keymap] 按键映射
    [Language] 语言
    [Languages] 语言
    [Project] 项目
    [Search] 搜索
    [Terminal] 终端
   *[other] { $source }
}
