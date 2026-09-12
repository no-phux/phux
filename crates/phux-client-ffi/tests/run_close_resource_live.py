#!/usr/bin/env python3
"""Run close_resource_live with the existing isolated, explicit-binary fixture.

Usage: run_close_resource_live.py /absolute/phux /absolute/close-live /scratch/root
The fixture uses its private detach-live session name for teardown compatibility.
"""
from run_detach_live import main

if __name__ == "__main__":
    main()
