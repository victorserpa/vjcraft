// Thin C shim over Syphon's Metal server/client, for vjc-net. Compiled with ARC.
#import <Cocoa/Cocoa.h>
#import <Metal/Metal.h>
#include <stdatomic.h>
#include <string.h>
#import "SyphonServerDirectory.h"
#import "SyphonMetalServer.h"
#import "SyphonMetalClient.h"

static id<MTLDevice> dev(void) {
    static id<MTLDevice> d;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ d = MTLCreateSystemDefaultDevice(); });
    return d;
}

// Syphon's directory and servers get their distributed notifications on the process's
// MAIN run loop (NSDistributedNotificationCenter delivers there only), so discovery needs
// the host to spin it; the egui/winit event loop does. Touching the directory early lets
// it request announcements.
static void ensure_directory(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{ (void)[SyphonServerDirectory sharedDirectory]; });
}

// Implemented (privately) in SyphonServerDirectory.m: normalizes a server's own description
// the way the directory does for announcements.
@interface NSDictionary (VjcSyphonPimped)
- (NSDictionary *)pimpedVersionForSyphon;
@end

static NSString *clean(id v) {
    NSString *s = [v isKindOfClass:[NSString class]] ? v : @"";
    return [[s stringByReplacingOccurrencesOfString:@"\t" withString:@" "]
        stringByReplacingOccurrencesOfString:@"\n" withString:@" "];
}

// Lines "uuid\tapp\tname\n". Free with vjc_syphon_free.
char *vjc_syphon_list(void) {
    ensure_directory();
    @autoreleasepool {
        NSMutableString *out = [NSMutableString string];
        for (NSDictionary *d in [[SyphonServerDirectory sharedDirectory] servers]) {
            [out appendFormat:@"%@\t%@\t%@\n", clean(d[SyphonServerDescriptionUUIDKey]),
                clean(d[SyphonServerDescriptionAppNameKey]), clean(d[SyphonServerDescriptionNameKey])];
        }
        return strdup([out UTF8String]);
    }
}

void vjc_syphon_free(char *p) { free(p); }

typedef struct {
    void *server;      // retained SyphonMetalServer
    void *queue;       // retained id<MTLCommandQueue>
    void *tex;         // retained id<MTLTexture>
    uint32_t w, h;
} VjcServer;

static void *server_new_here(const char *name) {
    @autoreleasepool {
        id<MTLDevice> d = dev();
        if (!d) return NULL;
        SyphonMetalServer *s = [[SyphonMetalServer alloc] initWithName:[NSString stringWithUTF8String:name]
                                                                device:d options:nil];
        if (!s) return NULL;
        VjcServer *v = calloc(1, sizeof(VjcServer));
        v->server = (void *)CFBridgingRetain(s);
        v->queue = (void *)CFBridgingRetain([d newCommandQueue]);
        return v;
    }
}

void *vjc_syphon_server_new(const char *name) {
    ensure_directory();
    return server_new_here(name);
}

int vjc_syphon_server_has_clients(void *p) {
    VjcServer *v = p;
    return [(__bridge SyphonMetalServer *)v->server hasClients] ? 1 : 0;
}

// Publishes tightly or loosely packed RGBA8 rows. Returns 0 on success.
int vjc_syphon_server_publish(void *p, const uint8_t *rgba, uint32_t w, uint32_t h, uint32_t stride) {
    VjcServer *v = p;
    if (!v || !rgba || w == 0 || h == 0) return 1;
    @autoreleasepool {
        id<MTLDevice> d = dev();
        if (!v->tex || v->w != w || v->h != h) {
            if (v->tex) CFRelease(v->tex);
            MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm
                                                                                          width:w height:h mipmapped:NO];
            td.usage = MTLTextureUsageShaderRead;
            id<MTLTexture> t = [d newTextureWithDescriptor:td];
            if (!t) { v->tex = NULL; return 2; }
            v->tex = (void *)CFBridgingRetain(t);
            v->w = w; v->h = h;
        }
        id<MTLTexture> t = (__bridge id<MTLTexture>)v->tex;
        [t replaceRegion:MTLRegionMake2D(0, 0, w, h) mipmapLevel:0 withBytes:rgba bytesPerRow:stride];
        id<MTLCommandQueue> q = (__bridge id<MTLCommandQueue>)v->queue;
        id<MTLCommandBuffer> cb = [q commandBuffer];
        [(__bridge SyphonMetalServer *)v->server publishFrameTexture:t onCommandBuffer:cb
                                                          imageRegion:NSMakeRect(0, 0, w, h) flipped:NO];
        [cb commit];
        return 0;
    }
}

void vjc_syphon_server_free(void *p) {
    VjcServer *v = p;
    if (!v) return;
    [(__bridge SyphonMetalServer *)v->server stop];
    if (v->tex) CFRelease(v->tex);
    CFRelease(v->queue);
    CFRelease(v->server);
    free(v);
}

typedef struct {
    void *client;
    // Heap counter shared with the frame handler block. Never freed: a late callback
    // from Syphon's queue may still run after client_free, so it must stay valid.
    _Atomic uint64_t *seq;
} VjcClient;

static void *client_with(NSDictionary *desc) {
    @autoreleasepool {
        id<MTLDevice> d = dev();
        if (!d || !desc) return NULL;
        _Atomic uint64_t *seq = calloc(1, sizeof(_Atomic uint64_t));
        SyphonMetalClient *c = [[SyphonMetalClient alloc] initWithServerDescription:desc device:d options:nil
            newFrameHandler:^(SyphonMetalClient *cl) { atomic_fetch_add(seq, 1); }];
        // The connection completes asynchronously: isValid may still be NO here.
        if (!c) { free(seq); return NULL; }
        VjcClient *v = calloc(1, sizeof(VjcClient));
        v->client = (void *)CFBridgingRetain(c);
        v->seq = seq;
        return v;
    }
}

void *vjc_syphon_client_new(const char *uuid) {
    ensure_directory();
    @autoreleasepool {
        NSString *u = [NSString stringWithUTF8String:uuid];
        for (NSDictionary *s in [[SyphonServerDirectory sharedDirectory] servers]) {
            if ([clean(s[SyphonServerDescriptionUUIDKey]) isEqualToString:u]) return client_with(s);
        }
        return NULL;
    }
}

// Connects straight to a server of this process, without the directory (tests).
void *vjc_syphon_client_new_for_server(void *srv) {
    VjcServer *v = srv;
    return client_with([[(__bridge SyphonMetalServer *)v->server serverDescription] pimpedVersionForSyphon]);
}

uint64_t vjc_syphon_client_seq(void *p) {
    VjcClient *v = p;
    return atomic_load(v->seq);
}

int vjc_syphon_client_valid(void *p) {
    VjcClient *v = p;
    return [(__bridge SyphonMetalClient *)v->client isValid] ? 1 : 0;
}

// Copies the current frame as BGRA8 (w*4 bytes per row) into dst (cap bytes).
// Returns 0 ok, 1 no frame, 2 buffer too small (w/h still set).
int vjc_syphon_client_read(void *p, uint8_t *dst, size_t cap, uint32_t *w, uint32_t *h) {
    VjcClient *v = p;
    @autoreleasepool {
        id<MTLTexture> t = [(__bridge SyphonMetalClient *)v->client newFrameImage];
        if (!t) return 1;
        *w = (uint32_t)t.width;
        *h = (uint32_t)t.height;
        size_t need = (size_t)t.width * t.height * 4;
        if (need > cap) return 2;
        [t getBytes:dst bytesPerRow:t.width * 4 fromRegion:MTLRegionMake2D(0, 0, t.width, t.height) mipmapLevel:0];
        return 0;
    }
}

void vjc_syphon_client_free(void *p) {
    VjcClient *v = p;
    if (!v) return;
    [(__bridge SyphonMetalClient *)v->client stop];
    CFRelease(v->client);
    // seq is intentionally leaked (8 bytes): the handler block may fire after stop.
    free(v);
}
