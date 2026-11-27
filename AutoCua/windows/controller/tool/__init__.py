# This file makes tool a Python package
from .open_app import open_on_windows, open_in_sandbox
from AutoCua.universal_tool.shell.service import ShellService

__all__ = ['open_on_windows', 'open_in_sandbox', 'ShellService']