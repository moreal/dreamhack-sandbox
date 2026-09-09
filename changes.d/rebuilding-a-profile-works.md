 -  **A profile that already has an image can be built again.** The second
    `wargame profile build` of any UTM profile failed at the very last step,
    after the guest had been provisioned and verified in full — UTM names
    each virtual machine's folder after the machine, and refused to create
    the new image under a name the old one still had. Every build now
    publishes under a name of its own, the build record is what identifies
    the image, and the image being replaced is deleted only once the record
    points at the new one.

 -  A publish that fails no longer leaves a virtual machine behind that
    nothing knows about. Whatever UTM registered is written into the build
    record first, so `wargame prune` leaves it alone and the next successful
    build cleans it up.
