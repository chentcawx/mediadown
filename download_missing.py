import re
import os
import urllib.request
import urllib.error
import time

CACHE_DIR = os.path.expanduser('~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/')
LOCK_PATH = r'D:\WorkBuddy\mediadown\media-down\src-tauri\Cargo.lock'
MIRROR = 'https://mirrors.huaweicloud.com/crates.io/crates'

# Read missing crates from file
missing = []
with open(r'D:\WorkBuddy\mediadown\media-down\missing_crates.txt') as f:
    for line in f:
        line = line.strip()
        if line:
            parts = line.split(' ')
            if len(parts) >= 2:
                name = parts[0]
                ver = parts[1]
                missing.append((name, ver))

print(f'Found {len(missing)} missing packages to download')

# Download each crate
success = 0
failed = []
for i, (name, ver) in enumerate(missing):
    pkg = f'{name}-{ver}'
    url = f'{MIRROR}/{name}/{pkg}.crate'
    dest = os.path.join(CACHE_DIR, f'{pkg}.crate')
    
    try:
        print(f'[{i+1}/{len(missing)}] Downloading {pkg}...', end=' ')
        urllib.request.urlretrieve(url, dest)
        size = os.path.getsize(dest)
        print(f'OK ({size//1024}KB)')
        success += 1
    except Exception as e:
        print(f'FAILED: {e}')
        failed.append((pkg, str(e)))
    
    # Small delay to be nice to the server
    time.sleep(0.1)

print(f'\nDownloaded {success}/{len(missing)} packages')
if failed:
    print(f'Failed {len(failed)} packages:')
    for pkg, err in failed[:10]:
        print(f'  - {pkg}: {err}')
