/* Guest eventfd/epoll lifetimes, one-shot rearming, readiness and fork sharing. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define C(n,a,b,c,d) call(n,(long)(a),(long)(b),(long)(c),(long)(d),0,0)
#define CHECK(x) do {if(!(x)){C(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
struct event {unsigned events;unsigned long data;} __attribute__((packed));
static long wait(long ep,struct event *out,int count,int timeout){return C(232,ep,out,count,timeout);}
static void put(long fd,unsigned long value){CHECK(C(1,fd,&value,8,0)==8);}
static unsigned long get(long fd){unsigned long value;CHECK(C(0,fd,&value,16,0)==8);return value;}
static volatile int seen;
static void handler(int signal) {CHECK(signal==10);seen++;}
__attribute__((naked)) static void restore(void) {__asm__ volatile("mov $15,%rax; syscall");}
__attribute__((noreturn)) void _start(void) {
    long fd=C(290,0,0x80800,0,0);CHECK(fd>=0);
    CHECK(C(72,fd,1,0,0)==1 && (C(72,fd,3,0,0)&0x803)==0x802);
    unsigned long value=0;CHECK(C(0,fd,&value,8,0)==-11 && C(0,fd,&value,0,0)==-22);
    value=~0ul;CHECK(C(1,fd,&value,8,0)==-22);value=~1ul;put(fd,value);
    value=1;CHECK(C(1,fd,&value,8,0)==-11 && get(fd)==~1ul);
    long page=call(9,0,4096,3,0x22,-1,0);CHECK(page>0);
    put(fd,2);CHECK(C(0,fd,page+4088,4096,0)==8 && *(unsigned long *)(page+4088)==2);
    CHECK(C(11,page,4096,0,0)==0);
    long duplicate=C(32,fd,0,0,0);CHECK(duplicate>=0 && C(72,duplicate,1,0,0)==0);
    long ep=C(291,0x80000,0,0,0);CHECK(ep>=0 && C(72,ep,1,0,0)==1);
    struct event ev={1,0x1122334455667788ul},out[4];
    CHECK(C(233,ep,1,fd,&ev)==0 && C(233,ep,1,fd,&ev)==-17);
    ev.data=22;CHECK(C(233,ep,1,duplicate,&ev)==0);
    CHECK(wait(ep,out,4,0)==0);
    put(fd,2);CHECK(wait(ep,out,1,0)==1);unsigned long first=out[0].data;
    CHECK(wait(ep,out,1,0)==1 && out[0].data!=first && out[0].events==1);
    CHECK(get(fd)==2 && wait(ep,out,4,0)==0);
    ev.events=0x40000001;ev.data=33;CHECK(C(233,ep,3,fd,&ev)==0);
    put(fd,2);CHECK(wait(ep,out,4,0)==2);CHECK(wait(ep,out,4,0)==1 && out[0].data==22);
    CHECK(C(233,ep,3,fd,&ev)==0 && wait(ep,out,4,0)==2);
    CHECK(C(233,ep,2,duplicate,0)==0);
    ev.events=1;CHECK(C(233,ep,3,fd,&ev)==0);
    CHECK(C(3,fd,0,0,0)==0 && wait(ep,out,4,0)==1 && out[0].data==33);
    long reused=C(290,0,0x800,0,0);CHECK(reused==fd);
    ev.data=44;CHECK(C(233,ep,1,reused,&ev)==0);
    CHECK(wait(ep,out,4,0)==1 && out[0].data==33);
    CHECK(C(3,duplicate,0,0,0)==0 && wait(ep,out,4,0)==0);
    put(reused,1);CHECK(wait(ep,(void*)1,4,0)==-14);CHECK(wait(ep,out,4,0)==1 && out[0].data==44);CHECK(get(reused)==1);
    CHECK(C(233,ep,2,reused,0)==0 && C(3,reused,0,0,0)==0);
    long semaphore=C(290,2,0x801,0,0);CHECK(semaphore>=0 && get(semaphore)==1 && get(semaphore)==1);
    CHECK(C(0,semaphore,&value,8,0)==-11 && C(3,semaphore,0,0,0)==0);
    int pipe[2];CHECK(C(293,pipe,0x80800,0,0)==0);
    CHECK(C(0,pipe[0],&value,1,0)==-11);
    CHECK(C(72,pipe[0],1,0,0)==1 && C(72,pipe[0],3,0,0)==0x800 && C(72,pipe[1],3,0,0)==0x801);ev.data=77;CHECK(C(233,ep,1,pipe[0],&ev)==0);
    struct { void *base; unsigned long length; } vectors[2]={{&value,1},{((char*)&value)+1,1}};
    CHECK(C(19,pipe[0],vectors,2,0)==-11);
    value=0x4241;CHECK(C(20,pipe[1],vectors,2,0)==2);value=0;
    CHECK(C(19,pipe[0],vectors,2,0)==2 && value==0x4241);
    static char fill[4096];long amount=0,r;
    while((r=C(1,pipe[1],fill,sizeof(fill),0))>0) amount+=r;
    CHECK(r==-11 && amount>=4096 && C(20,pipe[1],vectors,2,0)==-11);
    while(amount>0){r=C(0,pipe[0],fill,sizeof(fill),0);CHECK(r>0);amount-=r;}
    CHECK(C(0,pipe[0],fill,1,0)==-11);
    long reader=C(32,pipe[0],0,0,0);CHECK(reader>=0);
    CHECK(C(72,reader,4,0,0)==0 && C(72,pipe[0],3,0,0)==0 && C(72,pipe[1],3,0,0)==0x801);
    CHECK(C(3,pipe[0],0,0,0)==0);
    CHECK(C(1,pipe[1],"x",1,0)==1 && wait(ep,out,4,0)==1 && out[0].data==77);
    CHECK(C(3,reader,0,0,0)==0 && wait(ep,out,4,0)==0 && C(3,pipe[1],0,0,0)==0);
    long shared=C(284,0,0,0,0);CHECK(shared>=0);ev.data=55;CHECK(C(233,ep,1,shared,&ev)==0);
    long child=C(57,0,0,0,0);CHECK(child>=0);
    if(child==0){ev.data=66;CHECK(C(233,ep,3,shared,&ev)==0);put(shared,4);C(231,0,0,0,0);__builtin_unreachable();}
    CHECK(wait(ep,out,4,1000)==1 && out[0].data==66 && get(shared)==4);
    int status;CHECK(C(61,child,&status,0,0)==child && status==0);
    CHECK(wait(ep,out,4,1)==0);
    struct {void (*handler)(int);unsigned long flags;void (*restore)(void);unsigned long mask;} action={handler,0x14000000,restore,0};
    CHECK(C(13,10,&action,0,8)==0);
    unsigned long mask=1ul<<9,empty=0,previous=0;
    CHECK(C(14,0,&mask,0,8)==0 && C(62,C(39,0,0,0,0),10,0,0)==0 && seen==0);
    CHECK(call(281,ep,(long)out,4,-1,(long)&empty,8)==-4 && seen==1);
    CHECK(C(14,2,0,&previous,8)==0 && previous==mask);
    CHECK(call(281,ep,(long)out,4,0,(long)&empty,8)==0);
    CHECK(C(14,2,0,&previous,8)==0 && previous==mask);
    CHECK(C(14,2,&empty,0,8)==0);
    CHECK(C(233,ep,1,ep,&ev)==-22 && wait(ep,out,0,0)==-22);
    CHECK(C(3,shared,0,0,0)==0 && C(3,ep,0,0,0)==0);
    C(231,0,0,0,0);__builtin_unreachable();
}
