"""Synthetic stationary-observer negatives; no native or pixel qualification."""
import copy
import importlib.util
from pathlib import Path
import sys
import time
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import vm_strip_acceptance as proof
spec = importlib.util.spec_from_file_location('baseline_test', Path(__file__).with_name('test_vm_strip_acceptance.py'))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)


def sample():
    value = baseline.sample()
    value['owner']['fixture_mode'] = value['fixture']['fixture_mode'] = 'edge'
    role = copy.deepcopy(value['fixture']['windows']['gamma'])
    role['app_id'] = f"org.tuna.VmStrip.n{value['owner']['nonce']}.delta"
    role['color'] = list(proof.COLORS['delta'])
    value['fixture']['windows']['delta'] = role
    value['inventory']['state']['windows'].append({'id': 4, 'app_id': role['app_id'], 'rect': [1920, 48, 240, 160]})
    value['inventory']['state'].update(strip_offset=0.0, strip_view=0.0,
                                      pointer_position=[50.0, 65.0], tile_preview=None)
    for item in value['fixture']['windows'].values():
        item.update(move_requests=0, resize_requests=0, delivered_buttons=0, key_count=0)
    return value


def observations(before):
    result = []
    for index, (offset, live) in enumerate(((0, True), (100, True), (400, True), (900, False), (900, False), (900, False))):
        value = copy.deepcopy(before)
        state = value['inventory']['state']
        state.update(strip_offset=float(offset), strip_view=float(offset), strip_animating=live,
                     pointer_position=[1279.0, 65.0], rendered_frames=21 + min(index, 3),
                     tile_preview={'window': 1, 'rect': [1252, 48, 8, 160]})
        value['fixture']['sequence'] += index + 1
        value['fixture']['windows']['alpha']['move_requests'] = 1
        for item in state['windows']:
            item['rect'][0] -= offset
        result.append(value)
    return result


class Tests(unittest.TestCase):
    def run_observer(self, before, values, *, budget=1):
        iterator = iter(values)
        last = values[-1]
        with patch.object(proof.time, 'sleep', lambda _: None):
            return proof.await_stationary_edge(lambda _: next(iterator, last), before['owner'], before,
                                                deadline=time.monotonic() + budget)

    def test_live_demand_then_one_real_terminal_frame_with_three_steady_observations(self):
        before = sample()
        terminal, trace = self.run_observer(before, observations(before))
        self.assertFalse(terminal['inventory']['state']['strip_animating'])
        self.assertEqual(len(trace), 6)
        self.assertEqual(terminal['inventory']['state']['rendered_frames'], 24)
        # Three samples of ONE actual terminal frame; not fabricated extra frames.
        self.assertEqual({v['inventory']['state']['rendered_frames'] for v in trace[-3:]}, {24})
        with self.assertRaises(proof.NotReady):
            proof.windows(trace[1], before['owner'], edge=True)
        self.assertEqual(len(proof.windows(trace[1], before['owner'], edge=True, live_edge=True)), 4)

    def test_stationary_pointer_or_input_change_is_terminal(self):
        for mutation in ('pointer', 'keys', 'moves', 'bool_counter'):
            before = sample()
            values = observations(before)
            if mutation == 'pointer': values[1]['inventory']['state']['pointer_position'][0] -= 1
            elif mutation == 'keys': values[1]['fixture']['windows']['beta']['key_count'] += 1
            elif mutation == 'moves': values[1]['fixture']['windows']['alpha']['move_requests'] += 1
            else: values[1]['fixture']['windows']['alpha']['move_requests'] = True
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                self.run_observer(before, values)

    def test_identity_mode_mapping_and_sequence_regression_cannot_complete(self):
        for field in ('identity', 'mode', 'window', 'frame', 'sequence', 'lag'):
            before = sample()
            values = observations(before)
            if field == 'identity': values[1]['fixture']['identity']['start_ticks'] += 1
            elif field == 'mode': values[1]['fixture']['fixture_mode'] = 'baseline'
            elif field == 'window': values[1]['inventory']['state']['windows'][3]['id'] += 1
            elif field == 'frame': values[1]['inventory']['state']['rendered_frames'] = 1
            elif field == 'sequence': values[1]['fixture']['sequence'] = 1
            else: values[1]['inventory']['state']['strip_view'] = 3.5
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                self.run_observer(before, values)

    def test_missing_partial_role_cannot_be_terminal_and_early_stale_terminal_is_refused(self):
        before = sample()
        for failure in ('missing', 'stale', 'no_live', 'still_live'):
            values = observations(before)
            if failure == 'missing':
                for value in values: del value['fixture']['windows']['delta']
            elif failure == 'stale':
                for value in values[3:]: value['inventory']['state']['rendered_frames'] = 23
            elif failure == 'no_live':
                for value in values: value['inventory']['state']['strip_animating'] = False
            else:
                for value in values: value['inventory']['state']['strip_animating'] = True
            with self.subTest(failure=failure), self.assertRaises((RuntimeError, TimeoutError)):
                self.run_observer(before, values, budget=.05)

    def test_already_visible_target_and_late_read_fail_closed(self):
        before = sample()
        before['inventory']['state']['windows'][3]['rect'][0] = 1000
        with self.assertRaises(RuntimeError): self.run_observer(before, observations(before))
        before = sample()
        def late(_deadline):
            time.sleep(.03)
            return observations(before)[0]
        with self.assertRaises(TimeoutError):
            proof.await_stationary_edge(late, before['owner'], before, deadline=time.monotonic() + .01)

    def test_fixed_mode_and_no_input_capability_in_observer(self):
        before = sample()
        with self.assertRaises(RuntimeError): proof.windows(before, before['owner'])
        with self.assertRaises(RuntimeError): proof.windows(before, before['owner'], edge=1)
        with self.assertRaises(RuntimeError): proof.windows(before, before['owner'], live_edge=True)
        root = Path(__file__).resolve().parents[2]
        source = (root / 'scripts/lib/vm_strip_acceptance.py').read_text()
        observer = source.split('def await_stationary_edge(', 1)[1].split('def colored_frame(', 1)[0]
        for operation in ('input-send-event', 'send-key', 'transport.command', 'agent.run'):
            self.assertNotIn(operation, observer)
        self.assertIn("deadline = time.monotonic() + 120", source)
        self.assertLess(source.index("capture('07-vt-cancel'"), source.index('if edge:\n            before_edge'))
        self.assertIn('--strip-edge', (root / '.github/workflows/ci.yml').read_text())
        helper = (root / 'packaging/marlin/vm-lane/tuna-vm-lifecycle').read_text()
        self.assertIn('"--strip-edge-fixture" if mode == "edge" else "--strip-fixture"', helper)
        self.assertIn('value.get("fixture_mode") != mode', helper)


if __name__ == '__main__':
    unittest.main()
