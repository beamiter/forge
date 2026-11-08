#!/usr/bin/env python3
"""Bounded Q16 mask contracts; image tests never start a native application."""
from pathlib import Path
import random
import shutil
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import Mock

import reader


def packed(colors):
    return b''.join(struct.pack('<HHH', *color) for color in colors)


def boundary_colors():
    # Every 8-bit value on each side of every decision plane, plus Q16
    # immediate neighbors. Include known real antialiased rounding cases.
    colors = {(119*257, 185*257, 235*257), (136*257, 161*257, 236*257)}
    for value in range(0, 65536, 257):
        for delta in (-1, 0, 1):
            for other in (0, 25*257, 50*257, 80*257, 128*257, 32768, 65535):
                for r,g,b in ((value, other, 128*257+delta),
                              (value, other, value+80*257+delta),
                              (other, value, value+50*257+delta),
                              (value, value+25*257+delta, other)):
                    if all(0 <= x <= 65535 for x in (r,g,b)):
                        colors.add((r,g,b))
    rng = random.Random(42348116404)
    colors.update(tuple(rng.randrange(65536) for _ in range(3)) for _ in range(20000))
    return sorted(colors)


class FrostMaskBoundsTests(unittest.TestCase):
    def test_real_rounding_boundaries_do_not_become_false_white_pixels(self):
        # Inclusive integer arithmetic incorrectly turns these original black
        # antialias pixels white. FX normalizes before subtraction.
        colors = [(119*257,185*257,235*257), (136*257,161*257,236*257),
                  (0,40*257,200*257), (0,0,65535), (65535,65535,65535)]
        self.assertEqual(reader.frost_thumb_pixels(packed(colors), len(colors), 1, Mock()),
                         bytes((0,0,255,0,0)))

    def test_exact_byte_count_and_dimensions_are_required(self):
        for width,height,data in ((0,1,b''), (1,-1,b''), (True,1,b'\0'*6),
                (reader.FROST_MASK_MAX_PIXELS+1,1,b''), (1,1,b'\0'*5),
                (1,1,b'\0'*7), (2,1,b'\0'*6)):
            with self.subTest(width=width,height=height,size=len(data)):
                with self.assertRaises(RuntimeError):
                    reader.frost_thumb_pixels(data,width,height,Mock())

    def test_deadline_and_host_guard_errors_interrupt_instead_of_returning_mask(self):
        n = reader.FROST_MASK_CHUNK_PIXELS + 1
        for failure_at in (1,2,3,4):
            calls = []
            def checkpoint():
                calls.append(None)
                if len(calls) == failure_at: raise RuntimeError('deadline/host changed')
            with self.subTest(failure_at=failure_at), self.assertRaisesRegex(RuntimeError,'deadline/host'):
                reader.frost_thumb_pixels(b'\0'*(6*n),n,1,checkpoint)
            self.assertEqual(len(calls),failure_at)

    def test_each_chunk_uses_original_checkpoint_and_output_is_binary(self):
        n = reader.FROST_MASK_CHUNK_PIXELS + 1
        guard = Mock()
        result = reader.frost_thumb_pixels(b'\0'*(6*n),n,1,guard)
        self.assertEqual(result,b'\0'*n)
        self.assertEqual(guard.call_count,4)

    def test_partial_extraction_is_rejected_and_temporary_files_are_cleaned(self):
        with tempfile.TemporaryDirectory() as td:
            out = Path(td)/'test.thumb-mask.png'
            r = reader.Reader.__new__(reader.Reader)
            r.window_rect = [0,0,2,2]; r.checkpoint = Mock()
            def truncated(command):
                Path(next(x[4:] for x in command if x.startswith('RGB:'))).write_bytes(b'\0'*23)
                return '2 2 true'
            r.run = Mock(side_effect=truncated)
            with self.assertRaisesRegex(RuntimeError,'byte count'):
                r.frost_thumb_mask(Path('/unused/frame.png'),out)
            self.assertFalse(out.exists())
            self.assertFalse(out.with_suffix('.rgb16').exists())
            self.assertFalse(out.with_suffix('.pgm').exists())
            self.assertEqual(r.run.call_count,1)

    def test_unexpected_alpha_or_metadata_is_rejected_before_masking(self):
        for metadata in ('1 1 false', '', 'unexpected', '1 1 true\n1 1 true', '2 1 true'):
            with tempfile.TemporaryDirectory() as td:
                out = Path(td)/'mask.png'
                r = reader.Reader.__new__(reader.Reader); r.window_rect=[0,0,1,1]
                r.checkpoint=Mock()
                def extraction(command):
                    Path(next(x[4:] for x in command if x.startswith('RGB:'))).write_bytes(b'\0'*6)
                    return metadata
                r.run=Mock(side_effect=extraction)
                with self.subTest(metadata=metadata), self.assertRaisesRegex(RuntimeError,'dimensions or opacity'):
                    r.frost_thumb_mask(Path('/unused/frame.png'),out)
                self.assertEqual(list(Path(td).iterdir()),[])

    def test_mask_deadline_failure_leaves_no_partial_evidence(self):
        with tempfile.TemporaryDirectory() as td:
            out = Path(td)/'test.thumb-mask.png'
            r = reader.Reader.__new__(reader.Reader)
            r.window_rect = [0,0,1,1]
            r.checkpoint = Mock(side_effect=RuntimeError('Observation readiness deadline exhausted'))
            def extraction(command):
                Path(next(x[4:] for x in command if x.startswith('RGB:'))).write_bytes(b'\0'*6)
                return '1 1 true'
            r.run = Mock(side_effect=extraction)
            with self.assertRaisesRegex(RuntimeError,'deadline'):
                r.frost_thumb_mask(Path('/unused/frame.png'),out)
            self.assertEqual(list(Path(td).iterdir()),[])


@unittest.skipUnless(shutil.which('convert'), 'requires the existing ImageMagick tool')
class FrostMaskPixelEquivalenceTests(unittest.TestCase):
    def test_q16_threshold_planes_and_random_pixels_match_legacy_fx(self):
        colors = boundary_colors()
        colors += [(0,0,0)] * (-len(colors) % 256)
        width, height = 256, len(colors) // 256
        with tempfile.TemporaryDirectory() as td:
            p = Path(td); raw = p/'colors.rgb16'; raw.write_bytes(packed(colors))
            command = ['convert','-size','%dx%d'%(width,height),'-depth','16','-endian','LSB',
                       'RGB:'+str(raw),'+repage','-colorspace','sRGB','-fx',reader.FROST_THUMB_MASK,
                       '-colorspace','Gray','-depth','8','GRAY:'+str(p/'legacy.gray')]
            subprocess.run(command,check=True,capture_output=True,timeout=15)
            expected = (p/'legacy.gray').read_bytes()
            got = reader.frost_thumb_pixels(raw.read_bytes(),width,height,Mock())
            self.assertEqual(len(expected),len(colors))
            self.assertEqual(got,expected)

    def test_same_area_wrong_shape_and_multiple_frames_are_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            p=Path(td); source=p/'wrong-shape.png'; out=p/'mask.png'
            subprocess.run(['convert','-size','1x4','xc:rgb(0,40,200)',str(source)],
                           check=True,capture_output=True)
            r=reader.Reader.__new__(reader.Reader); r.window_rect=[0,0,2,2]; r.checkpoint=Mock()
            r.run=lambda c: subprocess.run(c,check=True,capture_output=True,text=True,timeout=15).stdout
            with self.assertRaisesRegex(RuntimeError,'dimensions or opacity'):
                r.frost_thumb_mask(source,out)
            self.assertFalse(out.exists())
            self.assertFalse(out.with_suffix('.rgb16').exists())
            # Two 2x1 frames have the expected total RGB byte count for 2x2,
            # but cannot be treated as one 2x2 captured frame.
            source=p/'multiple.miff'
            subprocess.run(['convert','-size','2x1','xc:rgb(0,40,200)',
                            '-size','2x1','xc:rgb(0,40,200)',str(source)],
                           check=True,capture_output=True)
            with self.assertRaisesRegex(RuntimeError,'dimensions or opacity'):
                r.frost_thumb_mask(source,out)
            self.assertFalse(out.exists())
            self.assertFalse(out.with_suffix('.rgb16').exists())

    def test_actual_transparent_chroma_is_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            p=Path(td); source=p/'transparent.png'; out=p/'mask.png'
            subprocess.run(['convert','-size','2x2','xc:rgba(0,40,200,0.5)',str(source)],
                           check=True,capture_output=True)
            r=reader.Reader.__new__(reader.Reader); r.window_rect=[0,0,2,2]; r.checkpoint=Mock()
            r.run=lambda c: subprocess.run(c,check=True,capture_output=True,text=True,timeout=15).stdout
            with self.assertRaisesRegex(RuntimeError,'dimensions or opacity'):
                r.frost_thumb_mask(source,out)
            self.assertFalse(out.exists())

    def test_srgb_normalization_and_virtual_page_do_not_change_mask_coordinates(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td); frame = p/'frame.png'; out = p/'thumb-mask.png'
            subprocess.run(['convert','-size','40x20','xc:rgb(0,40,200)',
                '-fill','rgb(119,185,235)','-draw','rectangle 8,4 30,10',
                '-set','page','1600x1200+200+175',str(frame)],check=True,capture_output=True)
            r = reader.Reader.__new__(reader.Reader); r.window_rect=[200,175,40,20]
            r.checkpoint=Mock()
            commands=[]
            def run(command):
                commands.append(command)
                result = subprocess.run(command,check=True,capture_output=True,timeout=15).stdout
                return result.decode() if command[-1] == 'info:' else result
            r.run=run; r.frost_thumb_mask(frame,out)
            run(['convert',str(frame),'+repage','-colorspace','sRGB',
                '-fx',reader.FROST_THUMB_MASK,'-colorspace','Gray','-depth','8','GRAY:'+str(p/'legacy.gray')])
            expected=(p/'legacy.gray').read_bytes()
            got=run(['convert',str(out),'-depth','8','GRAY:-'])
            self.assertEqual(got,expected)
            self.assertEqual(len(got),800)
            self.assertFalse(out.with_suffix('.rgb16').exists())
            self.assertFalse(out.with_suffix('.pgm').exists())
            self.assertIn('+repage',commands[0])


if __name__ == '__main__': unittest.main()
