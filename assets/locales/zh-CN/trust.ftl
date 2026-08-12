trust-project-header =
    { $count ->
        [1] 无法识别的项目
       *[other] 无法识别的项目（{ $count }）
    }
trust-restricted-description = 为保护系统，Flint 会在受限模式下打开不受信任的项目。
trust-review-settings = 请检查 .flint/settings.json 中由此项目配置的扩展或命令。
trust-prevents = 受限模式会阻止：
trust-prevents-settings = 应用项目设置
trust-prevents-language-servers = 运行语言服务器
trust-prevents-mcp = 安装 MCP 服务器集成
trust-stay-restricted = 保持受限模式
trust-continue = 信任并继续
trust-all-files = 信任所有单个文件
trust-all-folder = 信任 { $folder } 文件夹中的所有项目
trust-all-parents = 信任父文件夹中的所有项目
