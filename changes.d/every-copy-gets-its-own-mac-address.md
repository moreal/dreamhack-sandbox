 -  **Every copy of a UTM virtual machine now gets its own network address.**
    Copies inherited the original's MAC address, so two sessions launched
    from the same built image came up as the same machine on the same
    network: they answered to one IP, and a file copied into one could land
    in the other. Each copy — a session's VM, a published image, an
    installation VM adopted from a VM you already had — now gets a fresh,
    locally administered address before it is ever started.
