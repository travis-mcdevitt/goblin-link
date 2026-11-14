"""The Goblin Link platform for Hermes Agent."""


def register(ctx):
    """Hermes's plugin entry point. The adapter is imported here, not at the top, so that
    ``goblin_link.core`` can be imported (and tested) where Hermes is not installed."""
    from .adapter import register as register_adapter

    register_adapter(ctx)


__all__ = ["register"]
