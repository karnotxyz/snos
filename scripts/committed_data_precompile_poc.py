#!/usr/bin/env python3
"""Run the real Cairo OS committed_data handler as a component PIE, including hostile witnesses.

Requires Python 3.10 and cairo-lang==0.14.1a0. This is NOT a full SNOS block proof.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import zipfile

from Crypto.Hash import keccak
from starkware.cairo.common.poseidon_hash import poseidon_hash, poseidon_hash_many

HEIGHT = 19
DOMAIN = int.from_bytes(b'COMMITTED_DATA_V1', 'big')
PRIME = 2**251 + 17 * 2**192 + 1
COMMITTED_DATA_ADDRESS = int.from_bytes(
    keccak.new(digest_bits=256, data=b'committed_data_v1').digest(), 'big'
) & ((1 << 250) - 1)


def fixture():
    publisher, index, value = 12345, 499999, 312345000000
    # A fixed 19-level witness; siblings commit to other subtrees.
    siblings = [poseidon_hash(i, 42) for i in range(HEIGHT)]
    node = poseidon_hash_many([DOMAIN, publisher, index, value])
    for depth, sibling in enumerate(siblings):
        node = poseidon_hash(sibling, node) if (index >> depth) & 1 else poseidon_hash(node, sibling)
    return dict(address=COMMITTED_DATA_ADDRESS, root=node, publisher=publisher, index=index, value=value,
                response_value=value, siblings=siblings)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sequencer', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    tools = Path(sys.executable).parent
    program_file = args.output / 'committed_data-syscall.json'
    subprocess.run([str(tools / 'cairo-compile'),
        str(args.sequencer / 'experiments/committed-data/syscall_pie.cairo'),
        '--cairo_path', str(args.sequencer / 'crates/apollo_starknet_os_program/src/cairo'),
        '--output', str(program_file)], check=True)
    program = json.loads(program_file.read_text())
    # Python's runner cannot dispatch the Rust enum hint name. Replace ONLY its host-side
    # implementation; compiled Cairo instructions and constraints are byte-for-byte unchanged.
    replaced = 0
    for hints in program['hints'].values():
        for hint in hints:
            if hint['code'].strip() == 'LoadCommittedDataWitness':
                hint['code'] = "ids.committed_data_value = int(program_input['value'])\nids.committed_data_siblings = segments.gen_arg([int(x) for x in program_input['siblings']])"
                replaced += 1
    assert replaced == 1
    program_file.write_text(json.dumps(program))
    base = fixture()
    wire_witness = {k: v for k, v in base.items() if k not in ('address', 'response_value')}
    for key in ['root', 'publisher', 'value']:
        wire_witness[key] = hex(wire_witness[key])
    wire_witness['siblings'] = [hex(value) for value in wire_witness['siblings']]
    (args.output / 'witnesses.json').write_text(json.dumps([wire_witness], indent=2))
    cases = {'valid': base}
    for key in ['root', 'publisher', 'index', 'value', 'response_value']:
        cases['wrong_' + key] = dict(base, **{key: base[key] + 1})
    cases['wrong_sibling'] = copy.deepcopy(base)
    cases['wrong_sibling']['siblings'][9] += 1
    cases['short_path'] = dict(base, siblings=base['siblings'][:-1])
    cases['out_of_range_index'] = dict(base, index=2**HEIGHT)
    cases['negative_index'] = dict(base, index=PRIME - 1)
    full_felt = dict(base, value=PRIME - 1, response_value=PRIME - 1)
    node = poseidon_hash_many([DOMAIN, full_felt['publisher'], full_felt['index'], full_felt['value']])
    for depth, sibling in enumerate(full_felt['siblings']):
        node = poseidon_hash(sibling, node) if (full_felt['index'] >> depth) & 1 else poseidon_hash(node, sibling)
    full_felt['root'] = node
    cases['valid_full_felt'] = full_felt
    cases['wrong_address'] = dict(base, address=3)
    cases['legacy_address'] = dict(base, address=5)
    cases['wrong_selector'] = dict(base, selector=0)
    cases['insufficient_gas'] = dict(base, gas=999999)
    results = {}
    for name, value in cases.items():
        input_file = args.output / (name + '.json')
        input_file.write_text(json.dumps(value))
        cmd = [str(tools / 'cairo-run'), '--program', str(program_file),
               '--program_input', str(input_file), '--layout', 'recursive_with_poseidon',
               '--secure_run', '--print_info', '--print_output']
        if name == 'valid':
            cmd += ['--cairo_pie_output', str(args.output / 'committed_data-syscall.pie.zip')]
        result = subprocess.run(cmd, text=True, capture_output=True)
        (args.output / (name + '.log')).write_text(result.stdout + result.stderr)
        ok = result.returncode == 0
        # Failure must come from Cairo constraints, not a missing dependency or runner crash.
        rejected_by_cairo = 'Error at pc=' in result.stderr
        passed = ok if name.startswith('valid') else not ok and rejected_by_cairo
        results[name] = dict(passed=passed, accepted=ok, cairo_rejection=rejected_by_cairo)
        print(name, 'PASS' if passed else 'FAIL', flush=True)
        if not passed:
            print((result.stdout + result.stderr)[-2500:])
    pie = args.output / 'committed_data-syscall.pie.zip'
    if pie.exists():
        with zipfile.ZipFile(pie) as archive:
            resources = json.loads(archive.read('execution_resources.json'))
        assert resources['n_steps'] <= 4096, resources
        assert resources['n_memory_holes'] <= 32, resources
        assert resources['builtin_instance_counter']['poseidon_builtin'] <= 32, resources
        assert resources['builtin_instance_counter']['range_check_builtin'] <= 128, resources
    results['artifact'] = dict(kind='OS committed_data syscall component PIE, not full SNOS',
        sha256=hashlib.sha256(pie.read_bytes()).hexdigest() if pie.exists() else None)
    (args.output / 'results.json').write_text(json.dumps(results, indent=2))
    if not all(v['passed'] for k, v in results.items() if k != 'artifact'):
        raise SystemExit(1)

if __name__ == '__main__':
    main()
