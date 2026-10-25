/* FXSAVE/FXRSTOR register state, software tail, alignment and reserved MXCSR. */
static long call(long n,long a,long b,long c,long d) {register long r10 __asm__("r10")=d;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)) {call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
static unsigned char first[512] __attribute__((aligned(16))),second[512] __attribute__((aligned(16)));
static const unsigned long vector[2]={0x0123456789abcdeful,0xfedcba9876543210ul};
static const unsigned long numbers[2]={0xc000000000000000ul,0x4008000000000000ul};
static unsigned short control=0xb7f;static unsigned mxcsr=0x5f80;
static unsigned long get(unsigned char *p,int n) {unsigned long r=0;for(int i=0;i<n;i++)r|=(unsigned long)p[i]<<(8*i);return r;}
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    for(int n=0;n<512;n++){first[n]=0xa5;second[n]=0x5a;}
    __asm__ volatile("fninit; fldcw %3; ldmxcsr %4; fldl %1; fldl %2; movdqu %5,%%xmm0; movdqa %%xmm0,%%xmm15; fxsave64 %0"
        :"=m"(first):"m"(numbers[0]),"m"(numbers[1]),"m"(control),"m"(mxcsr),"m"(vector):"st","st(1)","xmm0","xmm15","memory");
    CHECK(get(first,2)==control && get(first+2,2)==0x3000 && first[4]==0xc0);
    CHECK(get(first+24,4)==mxcsr && (get(first+28,4)&0xffbf)==0xffbf);
    CHECK(get(first+32,8)==0xc000000000000000ul && get(first+40,2)==0x4000);
    CHECK(get(first+48,8)==0x8000000000000000ul && get(first+56,2)==0xc000);
    CHECK(get(first+160,8)==vector[0] && get(first+168,8)==vector[1]);
    CHECK(get(first+400,8)==vector[0] && get(first+408,8)==vector[1]);
    for(int n=464;n<512;n++)CHECK(first[n]==0xa5);
    __asm__ volatile("fninit; pxor %%xmm0,%%xmm0; pxor %%xmm15,%%xmm15; fxrstor64 %1; fxsave64 %0":"=m"(second):"m"(first):"st","st(1)","xmm0","xmm15","memory");
    for(int n=0;n<5;n++)CHECK(first[n]==second[n]);
    for(int n=24;n<32;n++)CHECK(first[n]==second[n]);
    for(int slot=0;slot<8;slot++)for(int n=0;n<10;n++)CHECK(first[32+slot*16+n]==second[32+slot*16+n]);
    for(int n=160;n<416;n++)CHECK(first[n]==second[n]);
    for(int n=464;n<512;n++)CHECK(second[n]==0x5a);
    /* The non-REX format retains the same implemented control/register state. */
    __asm__ volatile("fxrstor %1; fxsave %0":"=m"(second):"m"(first):"st","st(1)","xmm0","xmm15","memory");
    for(int n=0;n<5;n++)CHECK(first[n]==second[n]);
    for(int n=160;n<416;n++)CHECK(first[n]==second[n]);
    __asm__ volatile("fninit; fld1; movq %1,%%mm0; fxsave64 %0":"=m"(second):"m"(vector[0]):"st","mm0","memory");
    CHECK(second[4]==255 && (get(second+2,2)&0x3800)==0);
    CHECK(get(second+32,8)==vector[0] && get(second+40,2)==0xffff);
    CHECK(get(second+32+7*16,8)==0x8000000000000000ul && get(second+40+7*16,2)==0x3fff);
    __asm__ volatile("emms; fxsave64 %0":"=m"(second)::"memory");
    CHECK(second[4]==0 && (get(second+2,2)&0x3800)==0);
    __asm__ volatile("fld1; fxsave64 %0":"=m"(second)::"st","memory");
    CHECK(second[4]==0x80 && (get(second+2,2)&0x3800)==0x3800 && get(second+40,2)==0x3fff);
    for(int invalid=0;invalid<2;invalid++) {
        long child=call(57,0,0,0,0);CHECK(child>=0);
        if(child==0) {
            if(invalid==0) {unsigned char *p=first+1;__asm__ volatile("fxsave64 (%0)"::"r"(p):"memory");}
            else {first[27]|=0x80;__asm__ volatile("fxrstor64 %0"::"m"(first):"memory");}
            call(231,99,0,0,0);__builtin_unreachable();
        }
        int status;CHECK(call(61,child,(long)&status,0,0)==child && (status&127)==11);
    }
    call(231,0,0,0,0);__builtin_unreachable();
}
