# PyInstaller spec: builds dist/Yakusu/Yakusu.exe (one folder, starts fast).
# Run from the repo root after packaging/fetch_tools.ps1 has filled tools/.
import os
block_cipher = None
root = os.path.abspath('.')

a = Analysis(
    [os.path.join(root, 'packaging', 'launch.py')],
    pathex=[root],
    datas=[
        (os.path.join(root, 'yakusu', 'web'), 'yakusu/web'),
        (os.path.join(root, 'yakusu', 'data'), 'yakusu/data'),
        (os.path.join(root, 'tools'), 'tools'),
        (os.path.join(root, 'THIRD_PARTY.md'), '.'),
        (os.path.join(root, 'LICENSE'), '.'),
    ],
    hiddenimports=['webview'],
    cipher=block_cipher,
)
pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)
exe = EXE(pyz, a.scripts, [], exclude_binaries=True, name='Yakusu', console=False,
          icon=os.path.join(root, 'packaging', 'yakusu.ico') if os.path.exists(os.path.join(root, 'packaging', 'yakusu.ico')) else None)
coll = COLLECT(exe, a.binaries, a.zipfiles, a.datas, name='Yakusu')
