"""Negative controls for the independent history-enabled native identity lane."""
import importlib.util
from pathlib import Path
import runpy
import unittest

spec = importlib.util.spec_from_file_location('shell', Path(__file__).with_name('test-chat-shell-smoke.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def records():
    result = []
    for n, path in enumerate(['key', 'pst'], 1):
        token = f'owned-token-{n}'
        literal = '中文 e\u0301 ＡＢＣ ' * 8
        command = (f"printf '%s\\0' '{token}' '{literal}' "
                   '"$BASHPID" "$(tty)" "$__forge_command_id" "$BASH_COMMAND" > "$HOME/' + path + '"')
        execution_id = f'forge-bash-42-{n}'
        events = [dict(phase='start', execution_id=execution_id, parsed_command=command,
                       resolved_command=command, command_source='ShellReported', block_id=n),
                  dict(phase='end', execution_id=execution_id, correlation='MatchedUntrusted', exit_code=0),
                  dict(phase='record', execution_id=execution_id, command=command,
                       command_source='ShellReported', completion_provenance='ShellReported', block_id=n)]
        identity = dict(execution_id=execution_id, block_id=n, events=events,
                        expected_command=command, capsule_command=command, stored_command_exact=True)
        result.append(dict(path=path, token=token, pid=42, tty='/dev/pts/7',
                           capsule=dict(font='DejaVu Sans Mono 14', rows=4), metadata_identity=identity,
                           accepted_hex=b'\0'.join([token.encode(), literal.encode(), b'42', b'/dev/pts/7',
                                                     execution_id.encode(), command.encode(), b'']).hex()))
    return result


class MetadataOracles(unittest.TestCase):
    def test_reject_extra_or_reordered_global_lifecycles(self):
        sample = records()
        events = [event for record in sample for event in record['metadata_identity']['events']]
        m.metadata_events_acceptance(sample, events)
        for mutant in [events * 2, events[::-1], events[:-1], events + [dict(phase='start')]]:
            with self.assertRaises(RuntimeError):
                m.metadata_events_acceptance(sample, mutant)

    def test_exact_chain(self):
        m.shell_acceptance(records(), metadata=True)

    def reject(self, mutate):
        sample = records()
        mutate(sample)
        with self.assertRaises((RuntimeError, KeyError)):
            m.shell_acceptance(sample, metadata=True)

    def test_reject_missing_reordered_duplicate_events(self):
        for events in [[], records()[0]['metadata_identity']['events'][::-1],
                       records()[0]['metadata_identity']['events'] * 2]:
            self.reject(lambda r: r[0]['metadata_identity'].update(events=events))

    def test_reject_wrong_execution_at_each_boundary(self):
        for n in range(3):
            self.reject(lambda r: r[0]['metadata_identity']['events'][n].update(execution_id='foreign-id'))
        self.reject(lambda r: r[0]['metadata_identity'].update(execution_id='foreign-id'))

    def test_reject_screen_derived_or_unmatched_provenance(self):
        for n in [0, 2]:
            self.reject(lambda r: r[0]['metadata_identity']['events'][n].update(command_source='Screen'))
        self.reject(lambda r: r[0]['metadata_identity']['events'][2].update(completion_provenance='BoundaryInferred'))
        self.reject(lambda r: r[0]['metadata_identity']['events'][1].update(correlation='Mismatch'))
        self.reject(lambda r: r[0]['metadata_identity'].update(stored_command_exact=False))

    def test_reject_inserted_padding_and_incomplete_command(self):
        for field in ['expected_command', 'capsule_command']:
            for transform in [lambda c: c.replace('ＡＢＣ', 'ＡＢ Ｃ', 1), lambda c: c[:-1]]:
                self.reject(lambda r: r[0]['metadata_identity'].update({field: transform(r[0]['metadata_identity'][field])}))
        for n, field in [(0, 'parsed_command'), (0, 'resolved_command'), (2, 'command')]:
            self.reject(lambda r: r[0]['metadata_identity']['events'][n].update({field: ': wrong'}))

    def test_reject_wrong_capsule_id_or_status(self):
        for n in [0, 2]:
            self.reject(lambda r: r[0]['metadata_identity']['events'][n].update(block_id=999))
        self.reject(lambda r: r[0]['metadata_identity']['events'][1].update(exit_code=1))

    def test_reject_wrong_accepted_bytes(self):
        self.reject(lambda r: r[0].update(accepted_hex=r[0]['accepted_hex'].replace('e4b8ad', 'e4b8', 1)))
        self.reject(lambda r: r[0].update(accepted_hex=r[0]['accepted_hex'][:-2]))

    def test_disabled_history_oracle_remains_independent(self):
        sample = records()
        for record in sample:
            parts = bytes.fromhex(record['accepted_hex']).split(b'\0')
            record['accepted_hex'] = b'\0'.join(parts[:5] + [b'']).hex()
            identity = record['metadata_identity']
            # Execution ids still exist with history disabled. Exact logical
            # command metadata is absent and must never be required here.
            for field in ['expected_command', 'capsule_command', 'stored_command_exact']:
                del identity[field]
            for index in [0, 2]:
                identity['events'][index]['command_source'] = 'Screen'
        m.shell_acceptance(sample)
        with self.assertRaises(KeyError):
            m.shell_acceptance(sample, metadata=True)
        self.assertNotEqual(m.TEST, m.METADATA_TEST)

    def test_real_owned_bash_exposes_same_command_and_execution_id(self):
        probe = runpy.run_path(str(Path(__file__).with_name('test-bash-command-metadata.py')))
        command = records()[0]['metadata_identity']['expected_command'].replace('$HOME/key', '$HOME/value')
        values = {'value': None}
        observed = probe['observe']('HISTCONTROL=ignoreboth', [command], values)
        self.assertEqual(observed, [[command.encode()]])
        parts = values['value'].split(b'\0')
        self.assertEqual(len(parts), 7)
        self.assertEqual(parts[5], command.encode())
        self.assertRegex(parts[4].decode(), '^forge-bash-' + parts[2].decode() + '-[1-9][0-9]*$')
        self.assertEqual(parts[1], ('中文 e\u0301 ＡＢＣ ' * 8).encode())


if __name__ == '__main__':
    unittest.main()
