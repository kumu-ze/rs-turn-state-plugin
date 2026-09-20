"""Package an already-built native plugin; never includes runtime data."""
import hashlib
import json
import pathlib
import shutil
import sys

binary = pathlib.Path(sys.argv[1]).resolve(strict=True)
output = pathlib.Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=False)
shutil.copy2(binary, output / 'worker')
(output / 'worker').chmod(0o700)
manifest = {'id': 'turn-state', 'version': '0.2.3', 'apiVersion': 1,
            'menuLabel': '打标管理', 'author': 'kumu-ze', 'repository': 'https://github.com/kumu-ze/rs-turn-state-plugin', 'releaseAsset': 'rs-turn-state-{version}-linux-x64.tar.gz', 'executable': 'worker', 'capabilities': ['request.openai', 'provider.openai'],
            'files': {'worker': hashlib.sha256((output / 'worker').read_bytes()).hexdigest()}}
(output / 'plugin.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
print(output)
