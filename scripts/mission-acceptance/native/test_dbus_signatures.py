#!/usr/bin/env python3
"""Typed wire-signature regressions; no real bus, native app or provider runs.

The mock models dbus-python's non-introspected argument inference, including
plain Python int -> signed 'i'. Protocol signatures come from AT-SPI's public
Component.xml, Text.xml and Accessible.xml, also verified against the installed
libatk-bridge protocol XML, plus the standard D-Bus broker/Properties APIs:
https://gitlab.gnome.org/GNOME/at-spi2-core/-/tree/main/xml
https://dbus.freedesktop.org/doc/dbus-specification.html
"""
import unittest
from unittest.mock import patch

import observer
from test_observation_readiness import BusFixture


class Int32(int):
    signature = 'i'

    def __new__(cls, value):
        if not -(1 << 31) <= value < (1 << 31):
            raise OverflowError('D-Bus Int32 out of range')
        return super().__new__(cls, value)


class UInt32(int):
    signature = 'u'

    def __new__(cls, value):
        if not 0 <= value < (1 << 32):
            raise OverflowError('D-Bus UInt32 out of range')
        return super().__new__(cls, value)


class InvalidArgs(RuntimeError):
    """A protocol error, distinct from proven temporary observation changes."""
    def get_dbus_name(self):
        return 'org.freedesktop.DBus.Error.InvalidArgs'


def inferred_signature(args):
    result = []
    for arg in args:
        if isinstance(arg, (Int32, UInt32)):
            result.append(arg.signature)
        elif type(arg) is int:
            result.append('i')
        elif type(arg) is str:
            result.append('s')
        else:
            raise AssertionError('Unmodeled D-Bus argument: %r' % (arg,))
    return ''.join(result)


class SignatureBus(BusFixture):
    signatures = {
        ('org.a11y.Bus', 'GetAddress'): '',
        ('org.freedesktop.DBus', 'GetNameOwner'): 's',
        ('org.freedesktop.DBus', 'GetConnectionUnixProcessID'): 's',
        ('org.freedesktop.DBus.Properties', 'Get'): 'ss',
        (observer.ACCESSIBLE, 'GetChildren'): '',
        (observer.ACCESSIBLE, 'GetInterfaces'): '',
        (observer.ACCESSIBLE, 'GetRole'): '',
        (observer.ACCESSIBLE, 'GetState'): '',
        (observer.COMPONENT, 'GetExtents'): 'u',
        (observer.TEXT, 'GetText'): 'ii',
        (observer.TEXT, 'GetRangeExtents'): 'iiu',
    }

    def __init__(self):
        super().__init__()
        self.module.UInt32 = UInt32
        self.module.Int32 = Int32
        self.text = 'Mission\nrevision 1'
        self.wire_calls = []
        self.wire_error = None

    def endpoint(self, owner, path, interface):
        endpoint = super().endpoint(owner, path, interface)
        fixture = self

        class Endpoint:
            def __getattr__(self, method):
                def call(*args, timeout):
                    expected = fixture.signatures[(interface, method)]
                    actual = inferred_signature(args)
                    fixture.wire_calls.append((interface, method, args, actual, timeout))
                    if actual != expected:
                        raise InvalidArgs("Signature mismatch: got `%s`, expected `%s`" % (actual, expected))
                    if fixture.wire_error:
                        fixture.wire_error(interface, method, args)
                    return getattr(endpoint, method)(*args, timeout=timeout)
                return call
        return Endpoint()


class DBusSignatureTests(unittest.TestCase):
    def test_all_observer_calls_match_public_protocol_without_introspection(self):
        bus = SignatureBus()
        with patch('observer.time.monotonic', return_value=100):
            tree = bus.observe(deadline=100.25)
        self.assertIn(bus.text, tree['visible_text'])
        self.assertEqual({(interface, method) for interface, method, *_ in bus.wire_calls},
                         set(bus.signatures))
        self.assertTrue(all(0 < timeout <= .25 for *_, timeout in bus.wire_calls))
        text_reads = []
        ranges = []
        for interface, method, args, signature, _ in bus.wire_calls:
            if method == 'GetExtents':
                self.assertEqual(signature, 'u')
                self.assertIs(type(args[0]), UInt32)
                self.assertEqual(args[0], 0)
            elif method == 'GetText':
                self.assertEqual(signature, 'ii')
                self.assertTrue(all(type(arg) is Int32 for arg in args))
                text_reads.append(tuple(map(int, args)))
            elif method == 'GetRangeExtents':
                self.assertEqual(signature, 'iiu')
                self.assertEqual(tuple(map(type, args)), (Int32, Int32, UInt32))
                ranges.append(tuple(map(int, args)))
        # Both initial and final text reads must use signed offsets.
        self.assertEqual(text_reads, [(0, len(bus.text))] * 2)
        self.assertEqual(ranges, [(0, 7, 0), (8, 18, 0)])

    def test_original_untyped_component_coordinate_is_rejected(self):
        bus = SignatureBus()
        bus.module.UInt32 = int
        with self.assertRaisesRegex(InvalidArgs, 'got `i`, expected `u`') as caught:
            bus.observe()
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        self.assertEqual(sum(method == 'GetExtents' for _, method, *_ in bus.wire_calls), 1)

    def test_original_untyped_text_coordinate_is_rejected(self):
        bus = SignatureBus()
        text = bus.endpoint(bus.owner, '/button', observer.TEXT)
        with self.assertRaisesRegex(InvalidArgs, 'got `iii`, expected `iiu`'):
            text.GetRangeExtents(Int32(0), Int32(7), 0, timeout=.25)

    def test_unsigned_text_offsets_are_rejected(self):
        bus = SignatureBus()
        bus.module.Int32 = UInt32
        with self.assertRaisesRegex(InvalidArgs, 'got `uu`, expected `ii`') as caught:
            bus.observe()
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        text = bus.endpoint(bus.owner, '/button', observer.TEXT)
        with self.assertRaisesRegex(InvalidArgs, 'got `uuu`, expected `iiu`'):
            text.GetRangeExtents(UInt32(0), UInt32(7), UInt32(0), timeout=.25)

    def test_every_protocol_error_propagates_unchanged_as_a_hard_failure(self):
        for target in SignatureBus.signatures:
            with self.subTest(target=target):
                bus = SignatureBus()
                error = InvalidArgs('injected protocol failure: ' + '.'.join(target))

                def fail(interface, method, args):
                    if (interface, method) == target:
                        raise error

                bus.wire_error = fail
                with self.assertRaises(InvalidArgs) as caught:
                    bus.observe()
                self.assertIs(caught.exception, error)
                self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
                self.assertEqual(sum((interface, method) == target
                                     for interface, method, *_ in bus.wire_calls), 1)

    def test_late_protocol_error_wins_over_a_temporary_content_change(self):
        bus = SignatureBus()
        bus.latest_names['/button'] = 'Changed'
        error = InvalidArgs('late text protocol failure')
        reads = []

        def fail(interface, method, args):
            if method == 'GetText':
                reads.append(args)
                if len(reads) == 2:
                    raise error

        bus.wire_error = fail
        with self.assertRaises(InvalidArgs) as caught:
            bus.observe()
        self.assertIs(caught.exception, error)
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        self.assertEqual(len(reads), 2)


if __name__ == '__main__':
    unittest.main()
