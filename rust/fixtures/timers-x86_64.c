/* Wall/monotonic clocks, interrupted sleep, real timers, and fork inheritance. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;
}
#define CHECK(x) do {if(!(x)) {call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
struct time {long seconds,part;};
struct interval {struct time repeat,value;};
static volatile int signals;
static void handler(int signal) {if(signal==14) signals++;}
__attribute__((naked)) static void restore(void) {__asm__ volatile("mov $15,%rax; syscall");}
__attribute__((noreturn)) void _start(void) {
    struct time wall,mono,end,resolution;
    CHECK(call(228,0,(long)&wall,0,0)==0 && wall.seconds>0 && wall.part>=0 && wall.part<1000000000);
    CHECK(call(96,(long)&wall,0,0,0)==0 && wall.part>=0 && wall.part<1000000);
    long now=call(201,0,0,0,0);CHECK(now>=wall.seconds && now<=wall.seconds+1);
    CHECK(call(229,1,(long)&resolution,0,0)==0 && resolution.seconds==0 && resolution.part>0 && resolution.part<=1000000000);
    for(int clock=5;clock<=6;clock++) {
        CHECK(call(229,clock,(long)&resolution,0,0)==0 && resolution.seconds==0 && resolution.part>0);
        CHECK(call(228,clock,(long)&mono,0,0)==0 && mono.seconds>0 && mono.part>=0 && mono.part<1000000000);
    }
    CHECK(call(228,1,(long)&mono,0,0)==0);
    struct time delay={0,10000000};
    CHECK(call(35,(long)&delay,0,0,0)==0);
    CHECK(call(228,1,(long)&end,0,0)==0);
    CHECK((end.seconds-mono.seconds)*1000000000+end.part-mono.part>=10000000);
    /* Absolute sleeps in the past complete immediately. */
    CHECK(call(230,1,1,(long)&mono,0)==0);
    struct {void (*handler)(int);unsigned long flags;void (*restore)(void);unsigned long mask;} action={handler,0x4000000,restore,0};
    CHECK(call(13,14,(long)&action,0,8)==0);
    struct interval timer={{0,20000},{0,20000}},old,disable={{0,0},{0,0}};
    CHECK(call(38,0,(long)&timer,(long)&old,0)==0 && old.value.seconds==0 && old.value.part==0);
    delay.seconds=1;delay.part=0;struct time remaining={-1,-1};
    CHECK(call(35,(long)&delay,(long)&remaining,0,0)==-4 && signals>=1);
    CHECK(remaining.seconds==0 && remaining.part>0 && remaining.part<1000000000);
    CHECK(call(38,0,(long)&disable,(long)&old,0)==0 && old.repeat.seconds==0 && old.repeat.part==20000);
    CHECK(call(36,0,(long)&old,0,0)==0 && old.value.seconds==0 && old.value.part==0);
    CHECK(call(37,30,0,0,0)==0);
    long child=call(57,0,0,0,0);CHECK(child>=0);
    if(child==0) {
        CHECK(call(36,0,(long)&old,0,0)==0 && old.value.seconds==0 && old.value.part==0);
        call(231,0,0,0,0);__builtin_unreachable();
    }
    int status=-1;CHECK(call(61,child,(long)&status,0,0)==child && status==0);
    long left=call(37,0,0,0,0);CHECK(left>0 && left<=30 && call(37,0,0,0,0)==0);
    child=call(57,0,0,0,0);CHECK(child>=0);
    if(child==0) {
        action.handler=0;CHECK(call(13,14,(long)&action,0,8)==0);
        timer.repeat.part=0;timer.value.part=1000;
        CHECK(call(38,0,(long)&timer,0,0)==0);
        call(7,0,0,1000,0);
        call(231,99,0,0,0);__builtin_unreachable();
    }
    CHECK(call(61,child,(long)&status,0,0)==child && status==14);
    call(231,0,0,0,0);__builtin_unreachable();
}
