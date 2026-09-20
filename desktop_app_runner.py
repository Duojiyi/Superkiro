#!/usr/bin/env python3
"""
KIRO 智能极速接管中心 · 桌面独立窗口启动器入口 (Spec §9, T08)
"""
import os
import sys
import subprocess

def run_desktop_app():
    base_dir = os.path.dirname(os.path.abspath(__file__))
    run_desktop = os.path.join(base_dir, "run_desktop.py")
    cmd = [sys.executable, run_desktop] + sys.argv[1:]
    try:
        subprocess.run(cmd)
    except KeyboardInterrupt:
        pass

if __name__ == '__main__':
    run_desktop_app()
