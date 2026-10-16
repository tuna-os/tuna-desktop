/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Memory-fed PipeWire tone diagnostic. No file I/O in process callback. */
#include <pipewire/pipewire.h>
#include <spa/param/audio/format-utils.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <stdatomic.h>
struct player { struct pw_main_loop *loop; struct pw_stream *stream; uint8_t *data; size_t length; _Atomic size_t cursor; _Atomic unsigned invalid; unsigned errors; };
static void process(void *arg) {
 struct player *p=arg; struct pw_buffer *b=pw_stream_dequeue_buffer(p->stream);
 if(!b) return;
 struct spa_buffer *s=b->buffer;
 if(s->n_datas!=1 || !s->datas[0].data || !s->datas[0].chunk) { atomic_fetch_add(&p->invalid,1); pw_stream_queue_buffer(p->stream,b); return; }
 struct spa_data *d=&s->datas[0];
 size_t frames=d->maxsize/4;
 if(b->requested && b->requested<frames) frames=b->requested;
 size_t count=frames*4, pos=atomic_load(&p->cursor), left=p->length-pos, take=count<left?count:left;
 memcpy(d->data,p->data+pos,take); memset((uint8_t*)d->data+take,0,count-take);
 atomic_store(&p->cursor,pos+take); d->chunk->offset=0;d->chunk->stride=4;d->chunk->size=count;
 pw_stream_queue_buffer(p->stream,b);
}
static void state(void *arg,enum pw_stream_state old,enum pw_stream_state now,const char *error) {
 (void)old;struct player*p=arg;if(now==PW_STREAM_STATE_ERROR){fprintf(stderr,"stream error %s\n",error?error:"unknown");p->errors++;pw_main_loop_quit(p->loop);}
}
static void quit(void *arg,int sig){(void)sig;struct player*p=arg;pw_main_loop_quit(p->loop);}
static const struct pw_stream_events events={PW_VERSION_STREAM_EVENTS,.process=process,.state_changed=state};
static uint32_t le32(const uint8_t*p){return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);}
int main(int argc,char**argv){
 if(argc!=3) { return 2; }
 struct player p={0};uint8_t h[44];FILE*f=fopen(argv[2],"rb");if(!f)return 2;
 if(fread(h,1,44,f)!=44 || memcmp(h,"RIFF",4)||memcmp(h+8,"WAVEfmt ",8)||le32(h+16)!=16||h[20]!=1||h[22]!=2||le32(h+24)!=48000||h[34]!=16||memcmp(h+36,"data",4)){fclose(f);return 2;}
 p.length=le32(h+40);if(p.length>48000*60*4 || p.length%4 || !p.length){fclose(f);return 2;}
 p.data=malloc(p.length);if(!p.data){fclose(f);return 2;}if(fread(p.data,1,p.length,f)!=p.length){fclose(f);free(p.data);return 2;}fclose(f);
 pw_init(&argc,&argv);p.loop=pw_main_loop_new(NULL);if(!p.loop){free(p.data);return 2;}
 pw_loop_add_signal(pw_main_loop_get_loop(p.loop),SIGTERM,quit,&p);pw_loop_add_signal(pw_main_loop_get_loop(p.loop),SIGINT,quit,&p);
 p.stream=pw_stream_new_simple(pw_main_loop_get_loop(p.loop),"tuna-proof-memory-tone",pw_properties_new(PW_KEY_MEDIA_TYPE,"Audio",PW_KEY_CONFIG_NAME,"client-rt.conf",PW_KEY_MEDIA_CATEGORY,"Playback",PW_KEY_MEDIA_ROLE,"Music",PW_KEY_TARGET_OBJECT,argv[1],PW_KEY_NODE_NAME,"tuna-proof-memory-tone",NULL),&events,&p);
 if(!p.stream){pw_main_loop_destroy(p.loop);free(p.data);return 2;}
 uint8_t bytes[1024];struct spa_pod_builder builder=SPA_POD_BUILDER_INIT(bytes,sizeof(bytes));
 const struct spa_pod*params[]={spa_format_audio_raw_build(&builder,SPA_PARAM_EnumFormat,&SPA_AUDIO_INFO_RAW_INIT(.format=SPA_AUDIO_FORMAT_S16_LE,.rate=48000,.channels=2,.position={SPA_AUDIO_CHANNEL_FL,SPA_AUDIO_CHANNEL_FR}))};
 if(pw_stream_connect(p.stream,PW_DIRECTION_OUTPUT,PW_ID_ANY,PW_STREAM_FLAG_AUTOCONNECT|PW_STREAM_FLAG_MAP_BUFFERS|PW_STREAM_FLAG_RT_PROCESS,params,1)<0)p.errors++;else pw_main_loop_run(p.loop);
 pw_stream_disconnect(p.stream);pw_stream_destroy(p.stream);pw_main_loop_destroy(p.loop);pw_deinit();
 fprintf(stderr,"{\"played_frames\":%zu,\"invalid_buffers\":%u,\"errors\":%u}\n",atomic_load(&p.cursor)/4,atomic_load(&p.invalid),p.errors);
 free(p.data);return p.errors||atomic_load(&p.invalid)?1:0;
}
