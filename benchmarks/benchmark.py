"""Optional deterministic benchmarks; measurements are not pass/fail thresholds."""
import argparse
import gc
import json
import random
import sqlite3
import subprocess
import sys
import tempfile
import time
import tracemalloc
from contextlib import closing
from pathlib import Path

from orphanwiki._native import graph_counts
from orphanwiki.core import json_sha256, write_json


def native_benchmark():
    rng = random.Random(42)
    titles = [f'Article {i}' for i in range(50000)]
    links = [[titles[rng.randrange(len(titles))] for _ in range(20)] for _ in titles]
    start = time.perf_counter()
    incoming, outgoing, _ = graph_counts(titles, links, {})
    elapsed = time.perf_counter() - start
    assert sum(incoming) == sum(outgoing)
    print(f'50,000 nodes, 1,000,000 input edges; native + PyO3 conversion: {elapsed:.3f}s')


def json_benchmark():
    """Measure extra Python allocations after building the same synthetic input."""
    titles = [f'Voce sintetica è {i}' for i in range(10000)]
    value = {'pages': [{'page_id': i + 1, 'title': title,
                       'links': [titles[(i + j + 1) % len(titles)] for j in range(40)],
                       'categories': ['Tema sintetico'], 'created_at': None}
                      for i, title in enumerate(titles)]}
    with tempfile.TemporaryDirectory() as folder:
        streamed = Path(folder) / 'streamed.json'
        for name, action in [('write', lambda: write_json(streamed, value)),
                             ('checksum', lambda: json_sha256(value))]:
            gc.collect()
            tracemalloc.start()
            start = time.perf_counter()
            action()
            elapsed = time.perf_counter() - start
            _, peak = tracemalloc.get_traced_memory()
            tracemalloc.stop()
            print(f'{name}: extra Python allocation peak {peak / 2**20:.2f} MiB; {elapsed:.3f}s with tracing', flush=True)
        assert json.loads(streamed.read_text()) == value
        print(f'10,000 synthetic pages, 400,000 links; {streamed.stat().st_size / 2**20:.2f} MiB JSON. Peaks exclude input allocation and OS buffers.')


def disk_collection_benchmark():
    """Measure synthetic disk staging/export, without remote SQL or creator histories."""
    from orphanwiki.replicas import DiskPages
    for population in (10000, 50000):
        with tempfile.TemporaryDirectory() as folder, closing(sqlite3.connect(Path(folder) / 'stage.sqlite')) as stage:
            stage.executescript('PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;')
            gc.collect()
            tracemalloc.start()
            start = time.perf_counter()
            store = DiskPages(stage)
            for pid in range(1, population + 1):
                store[pid] = {'page_id': pid, 'title': f'Synthetic article {pid}', 'length_bytes': 1234}
                for offset in range(20):
                    store.add('links', pid, f'Synthetic article {(pid + offset) % population + 1}')
                store.add('categories', pid, 'Synthetic category')
            store.prepare_export()
            output = Path(folder) / 'raw.json'
            write_json(output, {'pages': []}, arrays={'pages': store.export_pages()})
            _, peak = tracemalloc.get_traced_memory()
            tracemalloc.stop()
            elapsed = time.perf_counter() - start
            print(f'{population:,} pages, {population * 20:,} edges: {peak / 2**20:.2f} MiB Python peak; '
                  f'{output.stat().st_size / 2**20:.2f} MiB JSON; {elapsed:.2f}s with tracing', flush=True)
    print('Excludes SQLite/native allocations, OS buffers, remote SQL, redirect/category hierarchies and creator processing; not total PAWS RAM.')


def comparison_run(folder):
    """Separate process: do not include fixture construction in measured RSS."""
    import resource
    from orphanwiki.core import compare_files
    from orphanwiki.__main__ import publish_comparison
    root = Path(folder)
    start = time.perf_counter()
    with compare_files(sorted(root.glob('*-raw.json')), work_dir=root) as results:
        publish_comparison(results, root / 'output')
    peak = max(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
               resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
    # ru_maxrss uses bytes on macOS, KiB on Linux. This is the largest process,
    # not the sum of parent/worker RSS or system page-cache consumption.
    scale = 2**20 if sys.platform == 'darwin' else 1024
    print(f'MEASURE {json.dumps({"seconds": time.perf_counter() - start, "largest_process_mib": peak / scale})}')


def comparison_benchmark():
    """Complete synthetic censuses with real candidate matching, no remote data."""
    population, degree = 5000, 20
    for languages in (['vec', 'lmo'], ['vec', 'lmo', 'fur', 'nap']):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for offset, language in enumerate(languages):
                rng = random.Random(42 + offset)
                base = offset % 2 * (population // 2)
                def pages():
                    for pid in range(1, population + 1):
                        yield {'page_id': pid, 'title': f'{language} synthetic {pid}',
                               'wikibase_item': f'Q{pid}', 'categories': [],
                               'links': [] if pid % 10 == 0 else
                                   [f'{language} synthetic {base + rng.randrange(population // 2) + 1}' for _ in range(degree)]}
                raw = {'metadata': {'language': language, 'source': f'Wiki Replicas / {language}wiki',
                                    'complete': True, 'demo': True,
                                    'started_at': '2026-01-01T00:00:00Z', 'finished_at': '2026-01-02T00:00:00Z',
                                    'item_mapping': {'source': 'page_props.wikibase_item', 'complete': True}},
                       'pages': [], 'redirects': {}}
                write_json(root / f'{language}-raw.json', raw, arrays={'pages': pages()})
            process = subprocess.run([sys.executable, '-m', 'benchmarks.benchmark', '--comparison-run', folder],
                                     check=True, capture_output=True, text=True)
            measurement = json.loads(next(line.removeprefix('MEASURE ') for line in process.stdout.splitlines()
                                          if line.startswith('MEASURE ')))
            index = json.loads((root / 'output/all-demo.json').read_text())
            assert sum(member['page_count'] for member in index['members']) == population * len(languages)
            assert all(len(member['metadata']['comparison']['reference_languages']) == len(languages) - 1
                       for member in index['members'])
            print(f'{len(languages)} complete synthetic wikis, {population:,} pages/wiki, '
                  f'{population * 9 // 10 * degree:,} input edges/wiki, '
                  f'{len(languages) * (len(languages) - 1)} ordered directions: '
                  f'{measurement["seconds"]:.2f}s; largest process {measurement["largest_process_mib"]:.2f} MiB', flush=True)
    print('Includes native analysis, checksums, indexed matching and JSON/CSV publication. '
          'Largest-process RSS excludes fixture generation, combined parent/worker RSS and OS caches. '
          'Not an estimate for arbitrary real Wikipedias.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--json', action='store_true', help='Measure serialization/checksum memory instead of the native graph')
    group.add_argument('--collection-memory', action='store_true', help='Measure synthetic SQLite staging/export')
    group.add_argument('--comparison-memory', action='store_true', help='Measure complete sequential file comparison')
    group.add_argument('--comparison-run', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.comparison_run:
        comparison_run(args.comparison_run)
    elif args.comparison_memory:
        comparison_benchmark()
    elif args.collection_memory:
        disk_collection_benchmark()
    elif args.json:
        json_benchmark()
    else:
        native_benchmark()
