/* Legacy 14/28-byte environments and 94/108-byte register save images. */
static long call(long n,long a,long b,long c){long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0);__builtin_unreachable();}} while(0)
static unsigned char first[128],second[128],environment[32];
static unsigned short control=0xb7f,after,status;
static unsigned short get(unsigned char *p){return p[0]|((unsigned)p[1]<<8);}
#define PREPARE() __asm__ volatile("fninit; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fninit; fldcw %0; fldz; fld1; fld1; ffree %%st(1)"::"m"(control):"st","st(1)","st(2)","memory")
#define ROUNDTRIP(prefix) __asm__ volatile(prefix " fnsave %0; fnstcw %2; fnstsw %3; " prefix " frstor %0; " prefix " fnsave %1":"=m"(first),"=m"(second),"=m"(after),"=m"(status)::"memory")
#define ENVIRONMENT(prefix) __asm__ volatile(prefix " fnstenv %0; fnstcw %1; " prefix " fldenv %0; fnstcw %2":"=m"(environment),"=m"(after),"=m"(status)::"memory")
__attribute__((noreturn)) void _start(void){
    for(int small=0;small<2;small++){
        int stride=small?2:4,size=small?94:108;
        for(int n=0;n<128;n++){first[n]=0xa5;second[n]=0x5a;}
        PREPARE();
        if(small){ROUNDTRIP("data16");}else{ROUNDTRIP("");}
        CHECK(after==0x37f && status==0);
        CHECK(get(first)==control && get(first+stride)==0x2800);
        /* R5=valid one, R6=empty, R7=zero; all other physical slots empty. */
        CHECK(get(first+2*stride)==0x73ff);
        for(int n=0;n<size;n++)CHECK(first[n]==second[n]);
        for(int n=size;n<128;n++)CHECK(first[n]==0xa5 && second[n]==0x5a);
        PREPARE();
        if(small){ENVIRONMENT("data16");}else{ENVIRONMENT("");}
        CHECK(after==(control|63) && status==control);
        CHECK(get(environment)==control && get(environment+stride)==0x2800 && get(environment+2*stride)==0x73ff);
        environment[2*stride]=0x34;environment[2*stride+1]=0x12;
        if(small){__asm__ volatile("data16 fldenv %0; data16 fnstenv %1"::"m"(environment),"m"(second):"memory");}
        else{__asm__ volatile("fldenv %0; fnstenv %1"::"m"(environment),"m"(second):"memory");}
        CHECK(get(second+2*stride)==0x4175);
        __asm__ volatile("fninit":::"memory");
    }
    call(231,0,0,0);__builtin_unreachable();
}
