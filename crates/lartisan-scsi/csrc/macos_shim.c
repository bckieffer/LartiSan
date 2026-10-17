// Thin C shim over the IOKit SCSITask user client (the COM-style plug-in interfaces are awkward
// to declare in Rust). Same calls as the proven lsprobe/lsburn C prototypes (not in this repository).
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/scsi/SCSITaskLib.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    SCSITaskDeviceInterface **dev;
    IOCFPlugInInterface **plugin;
    MMCDeviceInterface **mmc;
    io_service_t svc;
} ls_dev;

ls_dev *ls_open(char *err, size_t errlen) {
    CFMutableDictionaryRef match = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(match, CFSTR(kIOPropertySCSITaskDeviceCategory), CFSTR(kIOPropertySCSITaskAuthoringDevice));
    CFMutableDictionaryRef wrap = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(wrap, CFSTR(kIOPropertyMatchKey), match);
    CFRelease(match);

    io_iterator_t it;
    if (IOServiceGetMatchingServices(kIOMainPortDefault, wrap, &it) != KERN_SUCCESS) { snprintf(err, errlen, "IOServiceGetMatchingServices failed"); return NULL; }
    io_service_t svc = IOIteratorNext(it);
    IOObjectRelease(it);
    if (!svc) { snprintf(err, errlen, "no optical (SCSITask authoring) device found"); return NULL; }

    ls_dev *d = calloc(1, sizeof(ls_dev));
    d->svc = svc;
    SInt32 score;
    if (IOCreatePlugInInterfaceForService(svc, kIOMMCDeviceUserClientTypeID, kIOCFPlugInInterfaceID, &d->plugin, &score) != kIOReturnSuccess || !d->plugin) {
        snprintf(err, errlen, "IOCreatePlugInInterfaceForService failed"); IOObjectRelease(svc); free(d); return NULL;
    }
    if ((*d->plugin)->QueryInterface(d->plugin, CFUUIDGetUUIDBytes(kIOMMCDeviceInterfaceID), (LPVOID *)&d->mmc) != S_OK || !d->mmc) {
        snprintf(err, errlen, "QueryInterface(MMC) failed"); IODestroyPlugInInterface(d->plugin); IOObjectRelease(svc); free(d); return NULL;
    }
    d->dev = (*d->mmc)->GetSCSITaskDeviceInterface(d->mmc);
    if (!d->dev) { snprintf(err, errlen, "GetSCSITaskDeviceInterface failed"); (*d->mmc)->Release(d->mmc); IODestroyPlugInInterface(d->plugin); IOObjectRelease(svc); free(d); return NULL; }
    if ((*d->dev)->ObtainExclusiveAccess(d->dev) != kIOReturnSuccess) {
        snprintf(err, errlen, "ObtainExclusiveAccess failed (is a disc mounted? eject or unmount it)");
        (*d->dev)->Release(d->dev); (*d->mmc)->Release(d->mmc); IODestroyPlugInInterface(d->plugin); IOObjectRelease(svc); free(d); return NULL;
    }
    return d;
}

void ls_close(ls_dev *d) {
    if (!d) return;
    (*d->dev)->ReleaseExclusiveAccess(d->dev);
    (*d->dev)->Release(d->dev);
    (*d->mmc)->Release(d->mmc);
    IODestroyPlugInInterface(d->plugin);
    IOObjectRelease(d->svc);
    free(d);
}

// dir: 0 none, 1 data-in, 2 data-out. Returns 0 if the command was delivered (check *status),
// otherwise the IOReturn error. sense must hold 18 bytes.
int ls_exec(ls_dev *d, const uint8_t *cdb, uint8_t cdblen, int dir, uint8_t *buf, uint32_t buflen,
            uint32_t timeout_ms, uint8_t *status, uint8_t *sense, uint64_t *transferred) {
    SCSITaskInterface **task = (*d->dev)->CreateSCSITask(d->dev);
    if (!task) return -1;
    SCSITaskSGElement sg = {(mach_vm_address_t)(uintptr_t)buf, buflen};
    SCSI_Sense_Data sd;
    memset(&sd, 0, sizeof sd);
    SCSITaskStatus st = kSCSITaskStatus_No_Status;
    UInt64 got = 0;
    (*task)->SetCommandDescriptorBlock(task, (UInt8 *)cdb, cdblen);
    if (dir && buf && buflen)
        (*task)->SetScatterGatherEntries(task, &sg, 1, buflen, dir == 2 ? kSCSIDataTransfer_FromInitiatorToTarget : kSCSIDataTransfer_FromTargetToInitiator);
    else
        (*task)->SetScatterGatherEntries(task, NULL, 0, 0, kSCSIDataTransfer_NoDataTransfer);
    (*task)->SetTimeoutDuration(task, timeout_ms);
    IOReturn kr = (*task)->ExecuteTaskSync(task, &sd, &st, &got);
    (*task)->Release(task);
    if (kr != kIOReturnSuccess) return (int)kr;
    *status = (uint8_t)st;
    memcpy(sense, &sd, sizeof(sd) < 18 ? sizeof(sd) : 18);
    *transferred = got;
    return 0;
}
