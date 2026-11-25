# 字体构建

界面里嵌了两套字体，保证换一台机器显示完全一致：中文用 Noto Sans SC（思源黑体的 Google 发行版，SIL OFL 1.1），图标用 Tabler Icons（MIT）。嵌入之前先裁剪：中文只留界面用得上的字符集，图标只留 `icons.txt` 里列出的那些。

## 1. 下载输入

下载目录 `.tmp-dl/` 不入库，每个来源单独一个子目录。

**Noto Sans SC**（GitHub，直连不通时走代理 `curl -x http://127.0.0.1:7897`）：

```sh
mkdir -p .tmp-dl/notosans
curl -L -o .tmp-dl/notosans/NotoSansSC-VF.ttf \
  "https://github.com/google/fonts/raw/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"
curl -L -o .tmp-dl/notosans/OFL.txt \
  "https://github.com/google/fonts/raw/main/ofl/notosanssc/OFL.txt"
```

**Tabler Icons webfont**（npm 包 `@tabler/icons-webfont`，npmmirror 可直连）：

```sh
mkdir -p .tmp-dl/phosphor .tmp-dl/tabler-extract
curl -L -o .tmp-dl/phosphor/tabler-webfont.tgz \
  "https://registry.npmmirror.com/@tabler/icons-webfont/-/icons-webfont-<版本>.tgz"
tar -xzf .tmp-dl/phosphor/tabler-webfont.tgz -C .tmp-dl/tabler-extract \
  package/dist/fonts/tabler-icons.ttf package/dist/fonts/tabler-icons-filled.ttf \
  package/dist/tabler-icons.css package/dist/tabler-icons-filled.css package/LICENSE
```

## 2. 准备 fonttools

装在项目内的虚拟环境里，不动全局 Python：

```sh
python -m venv .tmp-dl/venv
.tmp-dl/venv/Scripts/python -m pip install fonttools
```

## 3. 构建

在项目根目录执行。`-I` 让解释器不加载当前目录和环境变量里的东西。

```sh
.tmp-dl/venv/Scripts/python.exe -I tools/fonts/build_fonts.py \
  --dl .tmp-dl --out assets/fonts \
  --rust crates/embdbg-app/src/icons.rs --icons tools/fonts/icons.txt
```

产物：

- `assets/fonts/NotoSansSC-Regular.subset.ttf`
- `assets/fonts/TablerIcons-Outline.subset.ttf`、`assets/fonts/TablerIcons-Filled.subset.ttf`
- `assets/fonts/LICENSE-*.txt`（随字体分发的许可证）
- `crates/embdbg-app/src/icons.rs`（生成的码点常量）

## 4. 新增图标

把图标名写进 `icons.txt`（不带 `ti-` 前缀），重新执行第 3 步，然后在界面代码里用 `crate::icons::outline::<名字>`。名字拼错时脚本会直接报错。
