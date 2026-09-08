 -  Challenge files are now checksummed again inside the guest. The host
    records a digest for everything it copies over and the guest recomputes
    each one before anything is unpacked, so a truncated or corrupted
    transfer is caught in the guest rather than silently solved around.
